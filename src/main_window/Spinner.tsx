import styles from "./Spinner.module.scss";

/** The busy indicator, sized to the text around it. */
export default function Spinner() {
  return <span className={styles.spinner} aria-hidden />;
}
