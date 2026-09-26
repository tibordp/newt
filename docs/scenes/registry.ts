import type { Scene } from "./types";

const modules = import.meta.glob<Scene>("./library/*.ts", {
  eager: true,
  import: "default",
});

/// Scene name (library file stem) → scene.
export const scenes: Record<string, Scene> = Object.fromEntries(
  Object.entries(modules).map(([path, scene]) => [
    path.replace(/^.*\/(.*)\.ts$/, "$1"),
    scene,
  ]),
);
