import { useState } from "react";

export function CreateDirectory({ path }: { path: string }) {
  const [name, setName] = useState("");

  return (
    <>
      <form onSubmit={(e) => e.preventDefault()}>
        <div className="dialog-contents">
          <h2>Create Directory</h2>
          <label htmlFor="path">Directory name</label>
          <input type="text" name="path" value={name} onChange={(e) => setName(e.target.value)} autoFocus />
        </div>
        <div className="dialog-buttons">
          <input type="submit" value="Create" />
          <input type="button" value="Cancel" />
        </div>
      </form>
    </>
  );
}
