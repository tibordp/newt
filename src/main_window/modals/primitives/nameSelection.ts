/// How much of a name a rename field preselects: a file's name up to its
/// extension, so typing replaces the name and keeps the type; a directory's
/// whole name, since a dot in it starts no extension.
export function editableNameEnd(name: string, isDir: boolean): number {
  if (isDir) return name.length;
  const dot = name.lastIndexOf(".");
  return dot > 0 ? dot : name.length;
}
