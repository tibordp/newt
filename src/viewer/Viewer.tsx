import { invoke } from "@tauri-apps/api/tauri";
import { message } from "@tauri-apps/api/dialog";
import { useEffect, useRef, useState } from "react";
import { useParams, useSearchParams } from "react-router-dom";

import "./Viewer.scss";
import { safeCommand } from "../lib/ipc";

function Viewer() {
  let [searchParams] = useSearchParams();
  let [contents, setContents] = useState("");

  const ref = useRef(null);
  const onkeydown = (e) => {
    if (e.key == "Escape") {
      safeCommand("close_window");
    } else {
      return;
    }
    e.preventDefault();
  };

  useEffect(() => {
    window.addEventListener("keydown", onkeydown);
    return () => window.removeEventListener("keydown", onkeydown);
  }, []);

  useEffect(() => {
    async function read_file() {
      const filename = searchParams.get("path");
      if (filename) {
        document.title = filename;
        try {
          const contents: string = await invoke("read_file", { filename });
          setContents(contents);
        } catch (e) {
          await message(e.toString(), {
            type: "error",
            title: "Error",
          });
        }
      }
    }

    read_file();
  }, [searchParams]);

  useEffect(() => {
    ref.current.focus();
  }, []);

  return (
    <div className="viewer" ref={ref} tabIndex={-1}>
      <pre>{contents}</pre>
    </div>
  );
}

export default Viewer;
