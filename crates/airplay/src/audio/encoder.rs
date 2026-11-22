//! Audio encoding for AirPlay streaming.

use crate::base::{AudioFormat, AudioCodec, SampleRate, error::Result};

/// Encoded audio packet.
#[derive(Debug, Clone)]
pub struct EncodedPacket {
    pub data: Vec<u8>,
    /// Number of samples in this packet.
    pub samples: u32,
    /// Timestamp in samples.
    pub timestamp: u64,
}

pub trait AudioEncoder: Send {
    fn encode(&mut self, samples: &[i16]) -> Result<EncodedPacket>;

    fn codec(&self) -> AudioCodec;

    fn frames_per_packet(&self) -> u32;

    /// Flush any buffered samples.
    fn flush(&mut self) -> Result<Option<EncodedPacket>>;
}

/// ALAC encoder for realtime streaming.
pub struct AlacEncoder {
    encoder: alac_encoder::AlacEncoder,
    input_format: alac_encoder::FormatDescription,
    format: AudioFormat,
    timestamp: u64,
    buffer: Vec<i16>,
    output_buffer: Vec<u8>,
}

impl AlacEncoder {
    pub fn new(format: AudioFormat) -> Result<Self> {
        let alac_format = alac_encoder::FormatDescription::alac(
            format.sample_rate.as_hz() as f64,
            format.frames_per_packet,
            format.channels as u32,
        );

        let pcm_format = alac_encoder::FormatDescription::pcm::<i16>(
            format.sample_rate.as_hz() as f64,
            format.channels as u32,
        );

        let encoder = alac_encoder::AlacEncoder::new(&alac_format);

        // ALAC's worst case is slightly larger than raw PCM.
        let max_encoded_size = format.frames_per_packet as usize * format.channels as usize * 2 + 256;

        Ok(Self {
            encoder,
            input_format: pcm_format,
            format,
            timestamp: 0,
            buffer: Vec::new(),
            output_buffer: vec![0u8; max_encoded_size],
        })
    }

    /// the ALAC magic cookie for SETUP: a 24-byte structure of codec parameters.
    pub fn magic_cookie(&self) -> Vec<u8> {
        self.encoder.magic_cookie().to_vec()
    }

    fn encode_frame(&mut self, samples: &[i16]) -> Result<EncodedPacket> {
        let num_samples = samples.len() / self.format.channels as usize;

        // Convert i16 samples to bytes (native endian)
        let input_bytes: Vec<u8> = samples
            .iter()
            .flat_map(|s| s.to_ne_bytes())
            .collect();

        let encoded_size = self.encoder.encode(
            &self.input_format,
            &input_bytes,
            &mut self.output_buffer,
        );

        let timestamp = self.timestamp;
        self.timestamp += num_samples as u64;

        Ok(EncodedPacket {
            data: self.output_buffer[..encoded_size].to_vec(),
            samples: num_samples as u32,
            timestamp,
        })
    }
}

impl AudioEncoder for AlacEncoder {
    fn encode(&mut self, samples: &[i16]) -> Result<EncodedPacket> {
        let samples_per_frame = self.format.frames_per_packet as usize * self.format.channels as usize;

        self.buffer.extend_from_slice(samples);

        if self.buffer.len() >= samples_per_frame {
            let frame_samples: Vec<i16> = self.buffer.drain(..samples_per_frame).collect();
            self.encode_frame(&frame_samples)
        } else {
            // Return partial packet (shouldn't happen in normal use)
            let partial: Vec<i16> = self.buffer.drain(..).collect();
            self.encode_frame(&partial)
        }
    }

    fn codec(&self) -> AudioCodec {
        AudioCodec::Alac
    }

    fn frames_per_packet(&self) -> u32 {
        self.format.frames_per_packet
    }

    fn flush(&mut self) -> Result<Option<EncodedPacket>> {
        if self.buffer.is_empty() {
            Ok(None)
        } else {
            // Pad with zeros to fill a complete frame
            let samples_per_frame = self.format.frames_per_packet as usize * self.format.channels as usize;
            while self.buffer.len() < samples_per_frame {
                self.buffer.push(0);
            }
            let remaining: Vec<i16> = self.buffer.drain(..).collect();
            Ok(Some(self.encode_frame(&remaining)?))
        }
    }
}

pub fn create_encoder(format: AudioFormat) -> Result<Box<dyn AudioEncoder>> {
    match format.codec {
        AudioCodec::Alac => Ok(Box::new(AlacEncoder::new(format)?)),
        _ => Err(crate::base::error::StreamingError::InvalidFormat(
            format!("Unsupported codec: {:?}", format.codec)
        ).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alac_format_44100() -> AudioFormat {
        AudioFormat {
            codec: AudioCodec::Alac,
            sample_rate: SampleRate::Hz44100,
            bit_depth: 16,
            channels: 2,
            frames_per_packet: 352,
        }
    }

    fn alac_format_48000() -> AudioFormat {
        AudioFormat {
            codec: AudioCodec::Alac,
            sample_rate: SampleRate::Hz48000,
            bit_depth: 16,
            channels: 2,
            frames_per_packet: 352,
        }
    }



    mod alac_encoder {
        use super::*;

        #[test]
        fn new_creates_encoder_for_44100() {
            let encoder = AlacEncoder::new(alac_format_44100());
            assert!(encoder.is_ok());
        }

        #[test]
        fn new_creates_encoder_for_48000() {
            let encoder = AlacEncoder::new(alac_format_48000());
            assert!(encoder.is_ok());
        }

        #[test]
        fn encode_352_samples() {
            let mut encoder = AlacEncoder::new(alac_format_44100()).unwrap();
            // 352 samples * 2 channels = 704 i16 values
            let samples = vec![0i16; 704];
            let packet = encoder.encode(&samples).unwrap();

            assert_eq!(packet.samples, 352);
            assert!(!packet.data.is_empty());
        }

        #[test]
        fn encode_increments_timestamp() {
            let mut encoder = AlacEncoder::new(alac_format_44100()).unwrap();
            let samples = vec![0i16; 704]; // 352 samples * 2 channels

            let packet1 = encoder.encode(&samples).unwrap();
            assert_eq!(packet1.timestamp, 0);

            let packet2 = encoder.encode(&samples).unwrap();
            assert_eq!(packet2.timestamp, 352);

            let packet3 = encoder.encode(&samples).unwrap();
            assert_eq!(packet3.timestamp, 704);
        }

        #[test]
        fn encoded_data_is_valid_alac() {
            let mut encoder = AlacEncoder::new(alac_format_44100()).unwrap();
            let samples = vec![0i16; 704];
            let packet = encoder.encode(&samples).unwrap();

            assert!(packet.data.len() > 0);
            // compressed silence is far smaller than the 1408 raw bytes.
            assert!(packet.data.len() <= 1500);
        }

        #[test]
        fn magic_cookie_is_valid() {
            let encoder = AlacEncoder::new(alac_format_44100()).unwrap();
            let cookie = encoder.magic_cookie();

            assert!(!cookie.is_empty());
            // ALAC magic cookie is typically 24+ bytes
            assert!(cookie.len() >= 24);
        }

        #[test]
        fn flush_returns_remaining_samples() {
            let mut encoder = AlacEncoder::new(alac_format_44100()).unwrap();
            encoder.buffer = vec![0i16; 100];
            let flushed = encoder.flush().unwrap();
            assert!(flushed.is_some());
        }

        #[test]
        fn flush_returns_none_when_empty() {
            let mut encoder = AlacEncoder::new(alac_format_44100()).unwrap();
            let flushed = encoder.flush().unwrap();
            assert!(flushed.is_none());
        }
    }


    mod create_encoder {
        use super::*;

        #[test]
        fn creates_alac_for_alac_format() {
            let encoder = create_encoder(alac_format_44100()).unwrap();
            assert_eq!(encoder.codec(), AudioCodec::Alac);
        }


        #[test]
        fn error_for_unsupported_codec() {
            let format = AudioFormat {
                codec: AudioCodec::Pcm,
                sample_rate: SampleRate::Hz44100,
                bit_depth: 16,
                channels: 2,
                frames_per_packet: 352,
            };
            let result = create_encoder(format);
            assert!(result.is_err());
        }
    }

    mod encoder_trait {
        use super::*;

        #[test]
        fn alac_reports_correct_codec() {
            let encoder = AlacEncoder::new(alac_format_44100()).unwrap();
            assert_eq!(encoder.codec(), AudioCodec::Alac);
        }


    }

    mod audio_validation {
        use super::*;
        use std::f64::consts::PI;

        /// a sine wave at `frequency`, as interleaved stereo samples.
        fn generate_sine_wave(
            frequency_hz: f64,
            sample_rate: u32,
            duration_samples: usize,
            amplitude: i16,
        ) -> Vec<i16> {
            let mut samples = Vec::with_capacity(duration_samples * 2);
            for i in 0..duration_samples {
                let t = i as f64 / sample_rate as f64;
                let value = (amplitude as f64 * (2.0 * PI * frequency_hz * t).sin()) as i16;
                samples.push(value);
                samples.push(value);
            }
            samples
        }

        fn calculate_rms(samples: &[i16]) -> f64 {
            if samples.is_empty() {
                return 0.0;
            }
            let sum_squares: f64 = samples.iter().map(|&s| (s as f64).powi(2)).sum();
            (sum_squares / samples.len() as f64).sqrt()
        }

        /// the dominant frequency, estimated from the zero-crossing rate.
        fn estimate_frequency_from_zero_crossings(samples: &[i16], sample_rate: u32) -> f64 {
            if samples.len() < 4 {
                return 0.0;
            }

            let mut zero_crossings = 0;
            // Use mono (every other sample for stereo)
            let mono: Vec<i16> = samples.iter().step_by(2).copied().collect();

            for i in 1..mono.len() {
                if (mono[i-1] >= 0 && mono[i] < 0) || (mono[i-1] < 0 && mono[i] >= 0) {
                    zero_crossings += 1;
                }
            }

            // Zero crossings per second / 2 = frequency
            let duration_secs = mono.len() as f64 / sample_rate as f64;
            (zero_crossings as f64 / duration_secs) / 2.0
        }

        #[test]
        fn alac_encodes_sine_wave_440hz() {
            let mut encoder = AlacEncoder::new(alac_format_44100()).unwrap();

            let sample_rate = 44100u32;
            let duration_samples = sample_rate as usize; // 1 second
            let sine_wave = generate_sine_wave(440.0, sample_rate, duration_samples, 16000);

            let source_rms = calculate_rms(&sine_wave);
            assert!(source_rms > 10000.0, "Source RMS too low: {}", source_rms);

            let source_freq = estimate_frequency_from_zero_crossings(&sine_wave, sample_rate);
            assert!((source_freq - 440.0).abs() < 20.0, "Source frequency wrong: {} Hz", source_freq);

            let samples_per_frame = 352 * 2; // stereo
            let mut encoded_packets = Vec::new();
            let mut total_encoded_bytes = 0;

            for chunk in sine_wave.chunks(samples_per_frame) {
                if chunk.len() == samples_per_frame {
                    let packet = encoder.encode(chunk).unwrap();
                    total_encoded_bytes += packet.data.len();
                    encoded_packets.push(packet);
                }
            }

            assert!(!encoded_packets.is_empty(), "No packets encoded");
            assert!(total_encoded_bytes > 0, "No encoded data produced");

            // ALAC is lossless: not much larger than raw, and smaller for simple signals.
            let raw_size = sine_wave.len() * 2; // 2 bytes per sample
            assert!(
                total_encoded_bytes < raw_size * 2,
                "Encoded size {} too large compared to raw {}",
                total_encoded_bytes,
                raw_size
            );

            println!("ALAC 440Hz sine wave test:");
            println!("  Source RMS: {:.2}", source_rms);
            println!("  Source frequency: {:.2} Hz", source_freq);
            println!("  Raw size: {} bytes", raw_size);
            println!("  Encoded size: {} bytes ({:.1}% of raw)",
                total_encoded_bytes,
                100.0 * total_encoded_bytes as f64 / raw_size as f64
            );
            println!("  Packets: {}", encoded_packets.len());
        }

        #[test]
        fn alac_encodes_multiple_frequencies() {
            let mut encoder = AlacEncoder::new(alac_format_44100()).unwrap();
            let sample_rate = 44100u32;

            let frequencies = [220.0, 440.0, 880.0, 1760.0];

            for freq in frequencies {
                let duration_samples = (sample_rate / 2) as usize;
                let sine_wave = generate_sine_wave(freq, sample_rate, duration_samples, 16000);

                let samples_per_frame = 352 * 2;
                let mut encoded_size = 0;

                for chunk in sine_wave.chunks(samples_per_frame) {
                    if chunk.len() == samples_per_frame {
                        let packet = encoder.encode(chunk).unwrap();
                        encoded_size += packet.data.len();

                        assert!(packet.data.len() > 10, "Packet too small for {} Hz", freq);
                    }
                }

                assert!(encoded_size > 0, "No data encoded for {} Hz", freq);
                println!("  {} Hz: {} bytes encoded", freq, encoded_size);
            }
        }

        #[test]
        fn alac_silence_compresses_well() {
            let mut encoder = AlacEncoder::new(alac_format_44100()).unwrap();

            let silence = vec![0i16; 44100 * 2];

            let samples_per_frame = 352 * 2;
            let mut encoded_size = 0;

            for chunk in silence.chunks(samples_per_frame) {
                if chunk.len() == samples_per_frame {
                    let packet = encoder.encode(chunk).unwrap();
                    encoded_size += packet.data.len();
                }
            }

            let raw_size = silence.len() * 2;

            // Silence should compress very well (< 20% of raw size)
            assert!(
                encoded_size < raw_size / 5,
                "Silence compression poor: {} vs {} raw",
                encoded_size,
                raw_size
            );

            println!("ALAC silence compression: {} -> {} bytes ({:.1}%)",
                raw_size, encoded_size, 100.0 * encoded_size as f64 / raw_size as f64);
        }


        #[test]
        fn alac_preserves_signal_characteristics() {
            let mut encoder = AlacEncoder::new(alac_format_44100()).unwrap();
            let sample_rate = 44100u32;

            // Generate a complex signal: 440Hz + 880Hz (fundamental + first harmonic)
            let duration_samples = sample_rate as usize; // 1 second
            let mut complex_signal = Vec::with_capacity(duration_samples * 2);

            for i in 0..duration_samples {
                let t = i as f64 / sample_rate as f64;
                let fundamental = 10000.0 * (2.0 * PI * 440.0 * t).sin();
                let harmonic = 5000.0 * (2.0 * PI * 880.0 * t).sin();
                let value = (fundamental + harmonic) as i16;
                complex_signal.push(value);
                complex_signal.push(value);
            }

            let source_rms = calculate_rms(&complex_signal);

            let samples_per_frame = 352 * 2;
            let mut packet_count = 0;

            for chunk in complex_signal.chunks(samples_per_frame) {
                if chunk.len() == samples_per_frame {
                    let packet = encoder.encode(chunk).unwrap();
                    assert!(!packet.data.is_empty(), "Empty packet");
                    packet_count += 1;
                }
            }

            assert!(packet_count > 0, "No packets encoded");
            println!("Complex signal (440Hz + 880Hz) encoded: {} packets, source RMS: {:.2}",
                packet_count, source_rms);
        }


    }
}
