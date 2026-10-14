/// Bidirectional formatting characters: embeddings and overrides
/// (U+202A–202E), isolates (U+2066–2069), and the LRM, RLM and ALM marks.
/// In a file name they reorder what is shown, so `photo<RLO>gpj.exe`
/// reads as `photoexe.jpg`.
const BIDI_CONTROLS = /[‪-‮⁦-⁩‎‏؜]/g;

/// `name` with each bidi formatting character shown as U+FFFD, one for
/// one, so offsets into the name still line up.
export function visibleName(name: string): string {
  return name.replace(BIDI_CONTROLS, "�");
}
