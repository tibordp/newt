#[derive(Debug, Clone, Copy)]
pub struct Size {
    row: u16,
    col: u16,
}

impl Size {
    #[must_use]
    pub fn new(row: u16, col: u16) -> Self {
        Self { row, col }
    }
}

impl From<Size> for nix::pty::Winsize {
    fn from(size: Size) -> Self {
        Self {
            ws_row: size.row,
            ws_col: size.col,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    }
}
