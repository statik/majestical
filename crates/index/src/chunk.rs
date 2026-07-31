//! Greedy transcript chunking for text embedding: windows of at most
//! `MAX_CHUNK_MS` and `MAX_CHUNK_WORDS`, never splitting a whisper segment
//! (an oversized single segment becomes one oversized chunk).
//!
//! The window is wall-clock span (`segment.end_ms - chunk.start_ms`), not
//! summed speech duration, so silence gaps between segments count toward
//! the cap. This is the right semantics: chunk timestamps drive playback
//! seek, so a chunk's span must reflect the video time it covers, not just
//! the time its speakers were talking.

use crate::transcribe::TranscriptSegment;

pub const MAX_CHUNK_MS: u64 = 45_000;
pub const MAX_CHUNK_WORDS: usize = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[must_use]
pub fn chunk_segments(segments: &[TranscriptSegment]) -> Vec<Chunk> {
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut current: Option<(Chunk, usize)> = None;
    for segment in segments {
        let words = segment.text.split_whitespace().count();
        match current.take() {
            None => {
                current = Some((
                    Chunk {
                        start_ms: segment.start_ms,
                        end_ms: segment.end_ms,
                        text: segment.text.trim().to_string(),
                    },
                    words,
                ));
            }
            Some((mut chunk, chunk_words)) => {
                let merged_ms = segment.end_ms.saturating_sub(chunk.start_ms);
                let merged_words = chunk_words + words;
                if merged_ms <= MAX_CHUNK_MS && merged_words <= MAX_CHUNK_WORDS {
                    chunk.end_ms = segment.end_ms;
                    chunk.text.push(' ');
                    chunk.text.push_str(segment.text.trim());
                    current = Some((chunk, merged_words));
                } else {
                    chunks.push(chunk);
                    current = Some((
                        Chunk {
                            start_ms: segment.start_ms,
                            end_ms: segment.end_ms,
                            text: segment.text.trim().to_string(),
                        },
                        words,
                    ));
                }
            }
        }
    }
    if let Some((chunk, _)) = current {
        chunks.push(chunk);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcribe::TranscriptSegment;
    use proptest::prelude::*;

    fn segment(start_ms: u64, end_ms: u64, words: usize) -> TranscriptSegment {
        TranscriptSegment {
            start_ms,
            end_ms,
            text: vec!["word"; words].join(" "),
        }
    }

    #[test]
    fn short_transcript_is_one_chunk() {
        let segments = vec![segment(0, 10_000, 20), segment(10_000, 20_000, 20)];
        let chunks = chunk_segments(&segments);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].start_ms, 0);
        assert_eq!(chunks[0].end_ms, 20_000);
        assert_eq!(chunks[0].text.split_whitespace().count(), 40);
    }

    #[test]
    fn duration_cap_splits() {
        let segments = vec![segment(0, 40_000, 10), segment(40_000, 80_000, 10)];
        let chunks = chunk_segments(&segments);
        assert_eq!(chunks.len(), 2, "40s + 40s cannot merge under the 45s cap");
    }

    #[test]
    fn word_cap_splits() {
        let segments = vec![segment(0, 1_000, 100), segment(1_000, 2_000, 100)];
        let chunks = chunk_segments(&segments);
        assert_eq!(
            chunks.len(),
            2,
            "100 + 100 words cannot merge under the 120-word cap"
        );
    }

    #[test]
    fn duration_boundary_45000_merges_45001_splits() {
        let at_cap = vec![segment(0, 20_000, 10), segment(20_000, 45_000, 10)];
        assert_eq!(
            chunk_segments(&at_cap).len(),
            1,
            "merged window exactly at MAX_CHUNK_MS (45_000) must merge"
        );

        let over_cap = vec![segment(0, 20_000, 10), segment(20_000, 45_001, 10)];
        assert_eq!(
            chunk_segments(&over_cap).len(),
            2,
            "merged window one ms over MAX_CHUNK_MS (45_001) must split"
        );
    }

    #[test]
    fn word_boundary_120_merges_121_splits() {
        let at_cap = vec![segment(0, 1_000, 60), segment(1_000, 2_000, 60)];
        assert_eq!(
            chunk_segments(&at_cap).len(),
            1,
            "merged word count exactly at MAX_CHUNK_WORDS (120) must merge"
        );

        let over_cap = vec![segment(0, 1_000, 60), segment(1_000, 2_000, 61)];
        assert_eq!(
            chunk_segments(&over_cap).len(),
            2,
            "merged word count one over MAX_CHUNK_WORDS (121) must split"
        );
    }

    #[test]
    fn silence_gap_counts_toward_the_duration_cap() {
        // Speech totals only 10s / 20 words — well under both caps — but the
        // wall-clock gap between the segments (5s..50s) pushes the merged
        // window past MAX_CHUNK_MS, so this must still split into 2 chunks.
        let segments = vec![segment(0, 5_000, 10), segment(50_000, 55_000, 10)];
        assert_eq!(
            chunk_segments(&segments).len(),
            2,
            "a >45s silence gap between segments must split, even though speech is short"
        );
    }

    #[test]
    fn one_oversized_segment_is_still_one_chunk() {
        // A single segment over both caps must never be split.
        let segments = vec![segment(0, 90_000, 300)];
        assert_eq!(chunk_segments(&segments).len(), 1);
    }

    #[test]
    fn empty_transcript_yields_no_chunks() {
        assert!(chunk_segments(&[]).is_empty());
    }

    proptest! {
        #[test]
        fn chunks_cover_every_segment_exactly_once_in_order(
            durations in proptest::collection::vec(1_u64..60_000, 0..40),
            words in proptest::collection::vec(1_usize..150, 0..40),
        ) {
            let count = durations.len().min(words.len());
            let mut segments = Vec::new();
            let mut clock = 0_u64;
            for i in 0..count {
                segments.push(segment(clock, clock + durations[i], words[i]));
                clock += durations[i];
            }
            let chunks = chunk_segments(&segments);
            // Coverage: total words in == total words out, order preserved.
            let words_in: usize = segments.iter().map(|s| s.text.split_whitespace().count()).sum();
            let words_out: usize = chunks.iter().map(|c| c.text.split_whitespace().count()).sum();
            prop_assert_eq!(words_in, words_out);
            // Boundaries: monotonically increasing, never overlapping.
            for window in chunks.windows(2) {
                prop_assert!(window[0].end_ms <= window[1].start_ms);
            }
            // Caps: any chunk holding >1 segment respects both caps.
            for chunk in &chunks {
                let chunk_words = chunk.text.split_whitespace().count();
                let single_segment = segments.iter().any(|s|
                    s.start_ms == chunk.start_ms && s.end_ms == chunk.end_ms);
                if !single_segment {
                    prop_assert!(chunk.end_ms - chunk.start_ms <= MAX_CHUNK_MS);
                    prop_assert!(chunk_words <= MAX_CHUNK_WORDS);
                }
            }
            if !segments.is_empty() {
                prop_assert_eq!(chunks[0].start_ms, segments[0].start_ms);
                prop_assert_eq!(chunks.last().unwrap().end_ms, segments.last().unwrap().end_ms);
            }
        }
    }
}
