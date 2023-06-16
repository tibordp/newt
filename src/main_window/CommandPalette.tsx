import { useCallback, useEffect } from "react";

type CommandPalleteProps = {
    open: boolean,
    onClose: () => void
}

export default function CommandPallete({ open, onClose }: CommandPalleteProps) {
    const onClick = useCallback((e) => {
        onClose();
        return false;
    }, [onClose])

    useEffect(() => {
        document.addEventListener("click", onClick);
        return () => {
            document.removeEventListener("click", onClick)
        }
    }, [onClick]);


    return <>
        {open &&
            <div className="command-pallete">
                <div className="command-pallete-content">foo</div>
            </div>}
    </>;

}
