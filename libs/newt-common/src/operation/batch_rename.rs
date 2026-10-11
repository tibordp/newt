use std::collections::HashSet;
use std::hash::{BuildHasher, Hasher};

use super::*;
use crate::batch_rename::{fold, plan};
use crate::vfs::PathStyle;

/// One folder of a batch rename.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct BatchRenameFolder {
    pub dir: VfsPath,
    /// (current name, new name).
    pub renames: Vec<(String, String)>,
}

pub(super) async fn execute_batch_rename(
    reporter: &mut ProgressReporter,
    context: &OperationContext,
    mut folders: Vec<BatchRenameFolder>,
    cancel: CancellationToken,
) -> Result<(), crate::Error> {
    let total: u64 = folders.iter().map(|f| f.renames.len() as u64).sum();
    reporter.send_prepared(0, total);
    // A folder's entries before the folder itself: deepest first.
    folders.sort_by_key(|f| std::cmp::Reverse(f.dir.path.components().count()));
    let mut done = 0;
    for folder in folders {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }
        let mut run = FolderRun::new(reporter, context, folder, &cancel, total, done).await?;
        let result = run.run().await;
        if result.is_err() {
            run.restore_parked().await;
        }
        done = run.done;
        result?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Pending,
    Done,
    Skipped,
}

struct Member {
    old: String,
    new: String,
    /// The name it has now.
    at: String,
    state: State,
    /// (device, inode), where the listing has them.
    identity: Option<(u64, u64)>,
}

struct FolderRun<'a> {
    reporter: &'a mut ProgressReporter,
    context: &'a OperationContext,
    cancel: &'a CancellationToken,
    dir: VfsPath,
    vfs: Arc<dyn Vfs>,
    members: Vec<Member>,
    exact_names: bool,
    style: PathStyle,
    /// The other names in the folder.
    others: HashSet<String>,
    /// Members at a temporary name.
    parked: Vec<usize>,
    total: u64,
    done: u64,
}

impl<'a> FolderRun<'a> {
    /// Look at the folder as it is now: members that are gone are skipped,
    /// and the listing says who has which name.
    async fn new(
        reporter: &'a mut ProgressReporter,
        context: &'a OperationContext,
        folder: BatchRenameFolder,
        cancel: &'a CancellationToken,
        total: u64,
        done: u64,
    ) -> Result<FolderRun<'a>, crate::Error> {
        let dir = context.registry.dereference(&folder.dir).await;
        let (vfs, _) = context.registry.resolve(&dir)?;
        let exact_names = vfs.descriptor().exact_names();
        let style = PathStyle::from_mount_meta(&vfs.mount_meta());
        let mut run = FolderRun {
            reporter,
            context,
            cancel,
            dir,
            vfs,
            members: Vec::new(),
            exact_names,
            style,
            others: HashSet::new(),
            parked: Vec::new(),
            total,
            done,
        };
        let listing = loop {
            match cancellable(cancel, run.vfs.list_files(&run.dir.path, None)).await {
                Ok(list) => break Some(list.files),
                Err(e) => {
                    match run
                        .reporter
                        .handle_io_error(e, &format!("Cannot list {}", run.dir), None, cancel, true)
                        .await?
                    {
                        IssueOutcome::Skip => break None,
                        IssueOutcome::Retry => {}
                    }
                }
            }
        };
        let Some(listing) = listing else {
            run.done += folder.renames.len() as u64;
            return Ok(run);
        };
        let by_name: HashMap<&str, &File> = listing
            .iter()
            .filter(|f| f.name != "..")
            .map(|f| (f.name.as_str(), f))
            .collect();
        let olds: HashSet<&str> = folder.renames.iter().map(|(old, _)| old.as_str()).collect();
        run.others = by_name
            .keys()
            .filter(|name| !olds.contains(*name))
            .map(|name| name.to_string())
            .collect();
        for (old, new) in folder.renames {
            let file = by_name.get(old.as_str());
            run.members.push(Member {
                identity: file.and_then(|f| f.device_id.zip(f.inode)),
                state: if file.is_some() {
                    State::Pending
                } else {
                    State::Skipped
                },
                at: old.clone(),
                old,
                new,
            });
        }
        Ok(run)
    }

    async fn run(&mut self) -> Result<(), crate::Error> {
        for i in 0..self.members.len() {
            if self.members[i].state == State::Skipped {
                let msg = format!("{} no longer exists", self.dir.join(&self.members[i].old));
                self.skip_with(i, msg).await?;
            }
        }
        // The preview refused these; the folder has changed since.
        let entries = self.entries();
        let problems = plan::check(&self.folder(&entries));
        for (i, problem) in problems.into_iter().enumerate() {
            if let Some(problem) = problem.filter(plan::Problem::is_error) {
                let msg = format!(
                    "Cannot rename {} to {}: {}",
                    self.dir.join(&self.members[i].old),
                    self.members[i].new,
                    describe(&problem, &entries),
                );
                self.skip_with(i, msg).await?;
            }
        }
        let entries = self.entries();
        let steps = plan::order(&self.folder(&entries));

        let mut deferred = Vec::new();
        for step in steps {
            // Never between a member's parking and its unparking.
            if self.parked.is_empty() && self.cancel.is_cancelled() {
                return Err(crate::Error::cancelled());
            }
            match step {
                plan::Step::Rename(i) | plan::Step::Unpark(i) => {
                    if !self.rename(i).await? {
                        deferred.push(i);
                    }
                }
                plan::Step::Park(i) => {
                    if !self.park(i).await? {
                        self.finish(i, State::Skipped);
                    }
                }
                plan::Step::Cycle(cycle) => self.cycle(&cycle).await?,
            }
        }
        self.retry(deferred).await
    }

    /// What the planner sees: the members still to rename, and the rest as
    /// staying where they are.
    fn entries(&self) -> Vec<(String, String)> {
        self.members
            .iter()
            .map(|m| match m.state {
                State::Pending => (m.old.clone(), m.new.clone()),
                State::Done | State::Skipped => (m.old.clone(), m.old.clone()),
            })
            .collect()
    }

    fn folder<'e>(&'e self, entries: &'e [(String, String)]) -> plan::Folder<'e> {
        plan::Folder {
            entries,
            others: Some(&self.others),
            style: self.style,
            exact_names: self.exact_names,
        }
    }

    /// Renames that found their name taken by a member that hadn't moved
    /// yet: the names didn't show it (an equivalence the fold doesn't
    /// model), so the filesystem orders them. A round without progress
    /// is a cycle: one member steps aside.
    async fn retry(&mut self, mut deferred: Vec<usize>) -> Result<(), crate::Error> {
        let mut stepped_aside = Vec::new();
        while !deferred.is_empty() {
            if self.cancel.is_cancelled() {
                return Err(crate::Error::cancelled());
            }
            let before = deferred.len();
            let mut blocked = Vec::new();
            for i in deferred {
                if !self.rename(i).await? {
                    blocked.push(i);
                }
            }
            deferred = blocked;
            if deferred.len() == before {
                let i = deferred.remove(0);
                if self.park(i).await? {
                    stepped_aside.push(i);
                } else {
                    self.finish(i, State::Skipped);
                }
            }
        }
        for i in stepped_aside {
            if !self.rename(i).await? {
                self.unpark_to_old(i).await;
                let msg = format!(
                    "Cannot rename {} to {}: another entry has the name",
                    self.dir.join(&self.members[i].old),
                    self.members[i].new
                );
                self.skip_with(i, msg).await?;
            }
        }
        Ok(())
    }

    /// Rename a member to its new name. `false` when a member that hasn't
    /// moved yet has the name.
    async fn rename(&mut self, i: usize) -> Result<bool, crate::Error> {
        if self.members[i].state != State::Pending {
            return Ok(true);
        }
        let to = self.members[i].new.clone();
        match self.move_to(i, &to).await? {
            Moved::Renamed | Moved::Copied => {
                self.parked.retain(|&p| p != i);
                self.finish(i, State::Done);
                Ok(true)
            }
            Moved::Skipped => {
                if self.parked.contains(&i) {
                    self.unpark_to_old(i).await;
                }
                self.finish(i, State::Skipped);
                Ok(true)
            }
            Moved::Blocked => Ok(false),
        }
    }

    /// Move a member to `to` through the shared rename, answering for the
    /// batch when something is in the way.
    async fn move_to(&mut self, i: usize, to: &str) -> Result<Moved, crate::Error> {
        let source = self.dir.join(&self.members[i].at);
        let target = self.dir.join(to);
        let mut occupants = Members {
            dir: &self.dir.path,
            members: &self.members,
            exact_names: self.exact_names,
        };
        // Boxed: it holds the whole copy fallback, too large to nest inline
        // through the batch's own futures.
        let moved = Box::pin(rename_entry(
            self.reporter,
            self.context,
            &source,
            &target,
            self.cancel,
            &mut occupants,
        ))
        .await?;
        match moved {
            Moved::Renamed => self.members[i].at = to.to_string(),
            Moved::Copied => {
                self.members[i].at = to.to_string();
                // The copy reported its own totals.
                self.reporter.send_prepared(0, self.total);
            }
            Moved::Skipped | Moved::Blocked => {}
        }
        Ok(moved)
    }

    /// Move a member to a temporary name beside it. `false` if the user
    /// skipped it.
    async fn park(&mut self, i: usize) -> Result<bool, crate::Error> {
        loop {
            let temp = temp_name();
            let from = self.dir.path.join(&self.members[i].at);
            match self
                .vfs
                .rename_no_replace(&from, &self.dir.path.join(&temp))
                .await
            {
                Ok(()) => {
                    self.members[i].at = temp;
                    self.parked.push(i);
                    return Ok(true);
                }
                Err(e) if e.kind == crate::ErrorKind::AlreadyExists => continue,
                Err(e) if e.kind == crate::ErrorKind::NotSupported => {
                    // No atomic refusal: the shared rename looks first, and
                    // nothing has an unguessable name.
                    return match self.move_to(i, &temp).await? {
                        Moved::Renamed | Moved::Copied => {
                            self.parked.push(i);
                            Ok(true)
                        }
                        Moved::Skipped | Moved::Blocked => Ok(false),
                    };
                }
                Err(e) => {
                    let msg = format!("Error renaming {}", self.dir.join(&self.members[i].at));
                    match self
                        .reporter
                        .handle_io_error(e, &msg, None, self.cancel, true)
                        .await?
                    {
                        IssueOutcome::Skip => return Ok(false),
                        IssueOutcome::Retry => {}
                    }
                }
            }
        }
    }

    /// Back from a temporary name to the old one, without asking anyone:
    /// nothing can have taken a name the batch vacated.
    async fn unpark_to_old(&mut self, i: usize) {
        let old = self.members[i].old.clone();
        let from = self.dir.path.join(&self.members[i].at);
        match self
            .vfs
            .rename_no_replace(&from, &self.dir.path.join(&old))
            .await
        {
            Ok(()) => self.members[i].at = old,
            Err(e) if e.kind == crate::ErrorKind::NotSupported => {
                if self
                    .vfs
                    .rename(&from, &self.dir.path.join(&old))
                    .await
                    .is_ok()
                {
                    self.members[i].at = old;
                }
            }
            Err(e) => warn!("batch rename: cannot move {} back to {}: {}", from, old, e),
        }
        self.parked.retain(|&p| p != i);
    }

    /// Return every parked member to its old name; for when the batch
    /// stops early.
    async fn restore_parked(&mut self) {
        for i in self.parked.clone() {
            self.unpark_to_old(i).await;
        }
    }

    /// Each member takes the next one's name, the last the first's.
    async fn cycle(&mut self, cycle: &[usize]) -> Result<(), crate::Error> {
        loop {
            match self.exchange_around(cycle).await {
                Ok(true) => break,
                Ok(false) => return self.cycle_through_temp(cycle).await,
                Err(e) => {
                    let names: Vec<&str> = cycle
                        .iter()
                        .map(|&i| self.members[i].old.as_str())
                        .collect();
                    let msg = format!("Error renaming {} in {}", names.join(", "), self.dir);
                    match self
                        .reporter
                        .handle_io_error(e, &msg, None, self.cancel, true)
                        .await?
                    {
                        IssueOutcome::Skip => {
                            for &i in cycle {
                                self.finish(i, State::Skipped);
                            }
                            return Ok(());
                        }
                        IssueOutcome::Retry => {}
                    }
                }
            }
        }
        // Each member now has the next one's old name; where that is only
        // a spelling of its new name (`b` for `B`), finish the spelling.
        for &i in cycle {
            if self.members[i].at == self.members[i].new {
                self.finish(i, State::Done);
            } else if !self.rename(i).await? {
                self.finish(i, State::Skipped);
            }
        }
        Ok(())
    }

    /// The cycle by atomic exchanges, k−1 for k members: exchanging the
    /// first slot with each other slot in turn leaves every member one slot
    /// on. `Ok(false)` where the backend can't exchange; an error leaves
    /// every member where it was.
    async fn exchange_around(&mut self, cycle: &[usize]) -> Result<bool, crate::Error> {
        let slots: Vec<String> = cycle.iter().map(|&i| self.members[i].at.clone()).collect();
        let path = |name: &str| self.dir.path.join(name);
        for k in 1..slots.len() {
            if let Err(e) = self.vfs.exchange(&path(&slots[0]), &path(&slots[k])).await {
                for undo in (1..k).rev() {
                    if let Err(e) = self
                        .vfs
                        .exchange(&path(&slots[0]), &path(&slots[undo]))
                        .await
                    {
                        warn!(
                            "batch rename: cannot undo an exchange in {}: {}",
                            self.dir, e
                        );
                    }
                }
                return if e.kind == crate::ErrorKind::NotSupported && k == 1 {
                    Ok(false)
                } else {
                    Err(e)
                };
            }
        }
        let n = cycle.len();
        for (j, &i) in cycle.iter().enumerate() {
            self.members[i].at = slots[(j + 1) % n].clone();
        }
        Ok(true)
    }

    /// The cycle through a temporary name: the first member steps aside,
    /// the rest follow each other from the last, the first takes the
    /// second's old name. Anything short of all of it is undone.
    async fn cycle_through_temp(&mut self, cycle: &[usize]) -> Result<(), crate::Error> {
        let first = cycle[0];
        if !self.park(first).await? {
            for &i in cycle {
                self.finish(i, State::Skipped);
            }
            return Ok(());
        }
        let mut moved = Vec::new();
        let mut complete = true;
        for &i in cycle[1..].iter().rev() {
            let to = self.members[i].new.clone();
            match self.move_to(i, &to).await? {
                Moved::Renamed | Moved::Copied => moved.push(i),
                Moved::Skipped | Moved::Blocked => {
                    complete = false;
                    break;
                }
            }
        }
        if complete {
            let to = self.members[first].new.clone();
            complete = matches!(
                self.move_to(first, &to).await?,
                Moved::Renamed | Moved::Copied
            );
        }
        if complete {
            self.parked.retain(|&p| p != first);
            for &i in cycle {
                self.finish(i, State::Done);
            }
            return Ok(());
        }
        for &i in moved.iter().rev() {
            let old = self.members[i].old.clone();
            if !matches!(self.move_to(i, &old).await?, Moved::Renamed | Moved::Copied) {
                warn!("batch rename: cannot move {} back in {}", old, self.dir);
            }
        }
        self.unpark_to_old(first).await;
        for &i in cycle {
            self.finish(i, State::Skipped);
        }
        Ok(())
    }

    async fn skip_with(&mut self, i: usize, msg: String) -> Result<(), crate::Error> {
        match self
            .reporter
            .raise_issue(IssueKind::IoError, msg, None, vec![IssueAction::Skip])
            .await?
        {
            IssueAction::Skip => {}
            _ => unreachable!("not offered"),
        }
        self.finish(i, State::Skipped);
        Ok(())
    }

    fn finish(&mut self, i: usize, state: State) {
        if self.members[i].state != State::Pending {
            return;
        }
        self.members[i].state = state;
        self.done += 1;
        self.reporter
            .maybe_send_progress(0, self.done, &self.members[i].new);
    }
}

/// The batch's members, as the shared rename asks about whatever is in
/// the way. Overwrite is only ever offered for what this calls `Outside`.
struct Members<'a> {
    dir: &'a Path,
    members: &'a [Member],
    exact_names: bool,
}

impl Members<'_> {
    fn classify(member: &Member) -> Occupant {
        match member.state {
            State::Pending => Occupant::Pending,
            State::Done | State::Skipped => Occupant::Settled,
        }
    }
}

#[async_trait::async_trait]
impl Occupants for Members<'_> {
    async fn occupant(&mut self, vfs: &dyn Vfs, path: &Path) -> Occupant {
        let Some(name) = path.file_name() else {
            return Occupant::Outside;
        };
        if self.exact_names {
            return self
                .members
                .iter()
                .find(|m| m.at == name)
                .map_or(Occupant::Outside, Self::classify);
        }
        // Identity where the listing had it: exact, whatever the fold
        // misses.
        if self.members.iter().all(|m| m.identity.is_some()) {
            return match vfs.file_info(path).await {
                Ok(file) => match file.device_id.zip(file.inode) {
                    Some(id) => self
                        .members
                        .iter()
                        .find(|m| m.identity == Some(id))
                        .map_or(Occupant::Outside, Self::classify),
                    None => Occupant::Pending,
                },
                // Can't tell: not Outside, which could offer to overwrite
                // a member.
                Err(_) => Occupant::Pending,
            };
        }
        let key = fold(name);
        for member in self.members.iter().filter(|m| fold(&m.at) == key) {
            if vfs
                .same_file(path, &self.dir.join(&member.at))
                .await
                .unwrap_or(false)
            {
                return Self::classify(member);
            }
        }
        Occupant::Outside
    }
}

fn describe(problem: &plan::Problem, entries: &[(String, String)]) -> String {
    match problem {
        plan::Problem::Empty => "the name is empty".into(),
        plan::Problem::DotName => "not a name".into(),
        plan::Problem::Separator => "the name has a separator".into(),
        plan::Problem::InvalidOnWindows(why) => format!("the name {why}"),
        plan::Problem::Duplicate { other } => {
            format!("{} would have the same name", entries[*other].0)
        }
        plan::Problem::Replaces | plan::Problem::MayReplace | plan::Problem::MayCollide { .. } => {
            unreachable!("warnings")
        }
    }
}

/// A hidden name nothing else has.
fn temp_name() -> String {
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    format!(".newt-rename-{:016x}", hasher.finish())
}
