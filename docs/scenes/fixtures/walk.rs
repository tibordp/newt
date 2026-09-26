//! Depth-first directory walk with cooperative cancellation.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use tokio::fs;

/// What the walker does with an entry after the visitor has seen it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Descend,
    Skip,
    Stop,
}

pub trait Visitor {
    fn visit(&mut self, path: &Path, is_dir: bool) -> Flow;
}

/// Walks `root` depth-first. Dropping the returned future cancels the walk
/// between two directory reads.
pub async fn walk(root: PathBuf, visitor: &mut impl Visitor) -> std::io::Result<u64> {
    let mut pending = VecDeque::from([root]);
    let mut visited = 0;

    while let Some(dir) = pending.pop_back() {
        let mut entries = fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let is_dir = entry.file_type().await?.is_dir();
            visited += 1;
            match visitor.visit(&entry.path(), is_dir) {
                Flow::Descend if is_dir => pending.push_back(entry.path()),
                Flow::Descend | Flow::Skip => {}
                Flow::Stop => return Ok(visited),
            }
        }
    }

    Ok(visited)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Count(usize);

    impl Visitor for Count {
        fn visit(&mut self, _: &Path, _: bool) -> Flow {
            self.0 += 1;
            Flow::Descend
        }
    }

    #[tokio::test]
    async fn counts_every_entry() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("a")).await.unwrap();
        fs::write(dir.path().join("a/b.txt"), "hi").await.unwrap();

        let mut count = Count(0);
        assert_eq!(walk(dir.path().into(), &mut count).await.unwrap(), 2);
        assert_eq!(count.0, 2);
    }
}
