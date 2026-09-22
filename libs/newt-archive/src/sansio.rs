//! The exchange model shared by the random-access readers: a state machine
//! hands the caller absolute byte ranges, the caller fetches them (in any
//! order, concurrently if it likes) and feeds the bytes back.

use std::ops::Range;

/// One fetched byte range, exactly as requested.
#[derive(Debug, Clone)]
pub struct Chunk {
    pub offset: u64,
    pub data: Vec<u8>,
}

/// Progress of a sans-IO operation. `Need` lists absolute archive byte ranges
/// the caller must fetch (they may be fetched concurrently) and feed to the
/// next `step` call; ranges are already validated to lie within the archive.
#[derive(Debug)]
pub enum Step<T> {
    Need(Vec<Range<u64>>),
    Done(T),
}
