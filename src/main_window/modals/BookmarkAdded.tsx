import { commands } from "../../lib/bindings";
import { safe } from "../../lib/ipc";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import {
  DialogShell,
  DialogHeader,
  DialogBody,
  DialogFooter,
} from "./primitives";

type BookmarkAddedProps = CommonDialogProps & ModalDataOf<"bookmark_added">;

/// Post-hoc acknowledgement of Mod+B, in the spirit of Chrome's bookmark
/// bubble: the bookmark is already saved, so Escape / click-away / Done all
/// keep it. Only Undo takes it back.
export default function BookmarkAdded({
  name,
  display_path,
  moved,
  cancel,
}: BookmarkAddedProps) {
  return (
    <DialogShell>
      <DialogHeader
        title={moved ? "Bookmark Moved to Top" : "Bookmark Added"}
        summary={name ?? display_path}
      />
      {name && <DialogBody>{display_path}</DialogBody>}
      <DialogFooter
        start={
          <button
            type="button"
            onClick={() => safe(commands.undoAddBookmark())}
          >
            Undo
          </button>
        }
      >
        <button type="button" className="suggested" onClick={cancel} autoFocus>
          Done
        </button>
      </DialogFooter>
    </DialogShell>
  );
}
