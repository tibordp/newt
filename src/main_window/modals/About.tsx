import { useState } from "react";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import {
  DialogShell,
  DialogHeader,
  DialogBody,
  DialogFooter,
} from "./primitives";
import styles from "./About.module.scss";
import appIcon from "../../assets/icon.png";

type AboutProps = CommonDialogProps & ModalDataOf<"about">;

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
    <DialogShell>
      <DialogHeader title="About Newt" srOnlyTitle />
      <DialogBody className={styles.body}>
        <img
          src={appIcon}
          alt="Newt"
          width={96}
          height={96}
          onClick={onIconClick}
          className={fact ? styles.iconTilted : styles.icon}
        />
        <div>
          <div className={styles.appName}>Newt</div>
          <div className={styles.tagline}>
            A keyboard-centric dual-pane file manager
          </div>
        </div>
        <div className={styles.buildInfo}>
          <div>{versionLine}</div>
          <div>{target_triple}</div>
        </div>
        <div className={styles.license}>
          <div>GNU General Public License v3.0 or later</div>
          <a
            href="https://github.com/tibordp/newt"
            target="_blank"
            rel="noopener"
          >
            github.com/tibordp/newt
          </a>
        </div>
        {fact && <div className={styles.factCard}>{fact}</div>}
      </DialogBody>
      <DialogFooter>
        <button type="button" onClick={cancel} autoFocus>
          Close
        </button>
      </DialogFooter>
    </DialogShell>
  );
}
