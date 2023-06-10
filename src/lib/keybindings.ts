export const modifiers = (e: React.KeyboardEvent<Element>) => {
    const isMac = navigator.platform.indexOf("Mac") === 0;
    const noModifiers = !e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey;
    let ctrlOrMeta;
    let insertKey;
    if (isMac) {
      ctrlOrMeta = e.metaKey;
      insertKey = "Help";
    } else {
      ctrlOrMeta = e.ctrlKey;
      insertKey = "Insert";
    }

    return {isMac, noModifiers, ctrlOrMeta, insertKey};
  }
