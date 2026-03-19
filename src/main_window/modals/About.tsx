import { useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { CommonDialogProps } from "./ModalContent";
import dialogStyles from "./Dialog.module.scss";
import appIcon from "../../assets/icon.png";

type AboutProps = CommonDialogProps & {
  version: string;
  git_revision?: string;
  build_date?: string;
  target_triple: string;
};

const NEWT_FACTS = [
  "Newts can regenerate lost limbs, eyes, and even parts of their heart.",
  "The rough-skinned newt produces tetrodotoxin, the same toxin found in pufferfish.",
  "Some newts can survive being frozen solid and thaw back to life.",
  "Newts navigate using Earth's magnetic field to find their home ponds.",
  "The word 'newt' comes from Middle English 'an ewte' — the 'n' migrated from the article.",
  "A group of newts is called a congress. No comment.",
  "Newts are the only vertebrates that can regenerate their eye lens.",
  "The great crested newt is so protected in the UK that moving one requires a license.",
  "Newts have been to space — Japanese scientists sent them to the ISS in 1994.",
  "Alpine newts can live at altitudes above 2,500 meters.",
  "Some newt species live up to 20 years in the wild.",
  "Newts breathe through their skin when underwater.",
];

export default function About({
  version,
  git_revision,
  build_date,
  target_triple,
  cancel,
}: AboutProps) {
  const [fact, setFact] = useState<string | null>(null);
  const [clickCount, setClickCount] = useState(0);

  const onIconClick = () => {
    const n = clickCount + 1;
    setClickCount(n);
    if (n >= 3) {
      setFact(NEWT_FACTS[Math.floor(Math.random() * NEWT_FACTS.length)]);
    }
  };

  const versionLine = [`v${version}`, git_revision && `(${git_revision})`]
    .filter(Boolean)
    .join(" ");

  return (
    <div>
      <div className={dialogStyles.dialogContents}>
        <Dialog.Title className="sr-only">About Newt</Dialog.Title>
        <div
          style={{
            display: "flex",
            flexDirection: "column",
            alignItems: "center",
            gap: "var(--space-4)",
            padding: "var(--space-4) 0",
          }}
        >
          <img
            src={appIcon}
            alt="Newt"
            width={96}
            height={96}
            onClick={onIconClick}
            style={{
              cursor: "default",
              userSelect: "none",
              transition: "transform 0.2s",
              transform: fact ? "rotate(5deg)" : undefined,
            }}
          />
          <div style={{ textAlign: "center" }}>
            <div style={{ fontSize: "1.3em", fontWeight: 700 }}>Newt</div>
            <div
              style={{
                color: "var(--color-fg-muted)",
                marginTop: "var(--space-1)",
              }}
            >
              A keyboard-centric dual-pane file manager
            </div>
          </div>
          <div
            style={{
              textAlign: "center",
              fontSize: "0.85em",
              color: "var(--color-fg-muted)",
              lineHeight: 1.6,
            }}
          >
            <div>{versionLine}</div>
            {build_date && <div>Built {build_date}</div>}
            <div>{target_triple}</div>
          </div>
          <div
            style={{
              fontSize: "0.85em",
              color: "var(--color-fg-muted)",
              textAlign: "center",
            }}
          >
            <div>GNU General Public License v2.0</div>
            <a
              href="https://github.com/tibordp/newt"
              target="_blank"
              rel="noopener"
              style={{ color: "var(--color-accent)" }}
            >
              github.com/tibordp/newt
            </a>
          </div>
          {fact && (
            <div
              style={{
                marginTop: "var(--space-2)",
                padding: "var(--space-3) var(--space-4)",
                background: "var(--color-surface)",
                borderRadius: "var(--radius-md)",
                fontSize: "0.85em",
                fontStyle: "italic",
                maxWidth: 320,
                textAlign: "center",
              }}
            >
              {fact}
            </div>
          )}
        </div>
      </div>
      <div className={dialogStyles.dialogButtons}>
        <button type="button" onClick={cancel} autoFocus>
          Close
        </button>
      </div>
    </div>
  );
}
