use rtrb::RingBuffer;

/// Creates a lock-free single-producer, single-consumer PCM ring. The reader waits until
/// `prefill` samples are buffered (the jitter buffer), and drops the oldest samples once more
/// than `max_buffered` are queued, so latency stays bounded when the producer runs fast.
pub fn pcm_ring(capacity: usize, prefill: usize, max_buffered: usize) -> (PcmWriter, PcmReader) {
    let (producer, consumer) = RingBuffer::new(capacity);
    let prefill = prefill.min(capacity);
    let reader = PcmReader {
        consumer,
        prefill,
        max_buffered: max_buffered.clamp(prefill, capacity),
        primed: false,
        underruns: 0,
    };
    (
        PcmWriter {
            producer,
            dropped: 0,
        },
        reader,
    )
}

#[derive(Debug)]
pub struct PcmWriter {
    producer: rtrb::Producer<f32>,
    dropped: u64,
}

impl PcmWriter {
    /// Queues as many samples as fit, returning how many were queued. Never blocks.
    pub fn push(&mut self, samples: &[f32]) -> usize {
        let (_, rejected) = self.producer.push_partial_slice(samples);
        self.dropped += rejected.len() as u64;
        samples.len() - rejected.len()
    }

    pub const fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn is_abandoned(&self) -> bool {
        self.producer.is_abandoned()
    }
}

#[derive(Debug)]
pub struct PcmReader {
    consumer: rtrb::Consumer<f32>,
    prefill: usize,
    max_buffered: usize,
    primed: bool,
    underruns: u64,
}

impl PcmReader {
    /// Fills `out` completely, with silence where no audio is buffered yet. Never blocks, so it
    /// is safe on a real-time audio thread. Returns the number of real samples written.
    pub fn fill(&mut self, out: &mut [f32]) -> usize {
        let buffered = self.consumer.slots();
        if !self.primed {
            if buffered < self.prefill.max(1) {
                out.fill(0.0);
                return 0;
            }
            self.primed = true;
        }
        if buffered > self.max_buffered {
            self.skip(buffered - self.prefill);
        }
        let (filled, missing) = self.consumer.pop_partial_slice(out);
        let written = filled.len();
        if !missing.is_empty() {
            missing.fill(0.0);
            self.primed = false;
            self.underruns += 1;
        }
        written
    }

    pub fn buffered(&self) -> usize {
        self.consumer.slots()
    }

    pub const fn underruns(&self) -> u64 {
        self.underruns
    }

    fn skip(&mut self, samples: usize) {
        if let Ok(chunk) = self.consumer.read_chunk(samples) {
            chunk.commit_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outputs_silence_until_the_jitter_buffer_is_primed() {
        let (mut writer, mut reader) = pcm_ring(64, 8, 32);
        let mut out = [1.0; 4];
        writer.push(&[0.5; 4]);
        assert_eq!(reader.fill(&mut out), 0);
        assert_eq!(out, [0.0; 4]);
        writer.push(&[0.5; 4]);
        assert_eq!(reader.fill(&mut out), 4);
        assert_eq!(out, [0.5; 4]);
    }

    #[test]
    fn underrun_pads_with_silence_and_reprimes() {
        let (mut writer, mut reader) = pcm_ring(64, 2, 32);
        writer.push(&[0.25; 3]);
        let mut out = [1.0; 4];
        assert_eq!(reader.fill(&mut out), 3);
        assert_eq!(out, [0.25, 0.25, 0.25, 0.0]);
        assert_eq!(reader.underruns(), 1);
        writer.push(&[0.25; 1]);
        assert_eq!(
            reader.fill(&mut out),
            0,
            "one sample is below the prefill again"
        );
    }

    #[test]
    fn caps_latency_by_dropping_the_oldest_audio() {
        let (mut writer, mut reader) = pcm_ring(64, 4, 8);
        let samples: Vec<f32> = (0..12u8).map(f32::from).collect();
        writer.push(&samples);
        let mut out = [0.0; 2];
        reader.fill(&mut out);
        assert_eq!(out, [8.0, 9.0]);
    }

    #[test]
    fn full_ring_drops_new_samples_without_blocking() {
        let (mut writer, reader) = pcm_ring(4, 1, 4);
        assert_eq!(writer.push(&[0.0; 6]), 4);
        assert_eq!(writer.dropped(), 2);
        assert_eq!(reader.buffered(), 4);
    }
}
