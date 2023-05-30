import { invoke } from "@tauri-apps/api/tauri";
import { message } from "@tauri-apps/api/dialog";

export const safeCommand = async (
  command: string,
  args: object = {}
): Promise<void> => {
  try {
    const contents: string = await invoke(command, { ...args });
  } catch (e) {
    await message(e.toString(), {
      type: "error",
      title: "Error",
    });
  }
};
