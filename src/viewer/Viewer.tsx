import { invoke } from "@tauri-apps/api/tauri";
import { message } from "@tauri-apps/api/dialog";
import { useEffect, useState } from "react";
import { useParams, useSearchParams } from "react-router-dom";

import "./Viewer.css";

function Viewer() {
  let [searchParams] = useSearchParams();
  let [contents, setContents] = useState("");

  const onkeydown = (e) => {
    if (e.key == "Escape") {
      window.close();
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

  return (
    <div className="viewer">
      <pre>{contents}</pre>
    </div>
  );
}

export default Viewer;
