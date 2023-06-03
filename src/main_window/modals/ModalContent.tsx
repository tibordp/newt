import { CreateDirectory } from "./CreateDirectory";

import "./ModalContent.css";

export type ModalState = {
  type: string;
  data: any;
};

export function ModalContent({ state }) {
  switch (state?.type) {
    case "create_directory":
      return <CreateDirectory {...state.data} />;
    default:
      return null;
  }
}
