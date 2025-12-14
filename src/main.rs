use std::io::stdin;
use std::process::id;
use std::{f64::consts::PI, ops::DerefMut};

use anyhow::Context;
use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Stream, StreamConfig, default_host};
use midir::{Ignore, MidiInput, MidiInputConnection, os::unix::VirtualInput};
use rtrb::{Consumer, Producer, RingBuffer};
use wmidi::MidiMessage;

const PORT_NAME: &str = "Rosadele Synth";

struct RosedaleParams {
    //The static pressure in the plenum
    max_pressure: f64,
    //How quickly the fan refills the plenum
    refill_speed: f64,
    valve_flow_rate: f64,
    pulse_duty_cycle: f64,
    //The sensitivity of the pitch to pressure curve
    pitch_modulation_depth: f64,

    //A meta-parameter for adjusting to abnormal tuning
    //in reference data
    tuning_multiplier: f64,

    //The point at which the chassis starts absorbing sound waves
    filter_cutoff: f64,
    //The speed the valves close
    spring_speed: f64,
}

impl Default for RosedaleParams {
    fn default() -> Self {
        RosedaleParams {
            max_pressure: 1.0,
            refill_speed: 10.0,
            valve_flow_rate: 0.6,
            pulse_duty_cycle: 0.3,
            pitch_modulation_depth: 0.06,

            tuning_multiplier: 1.0,

            filter_cutoff: 1500.0,
            spring_speed: 25.0,
        }
    }
}

struct PlenumPressure {
    pressure: f64,
}

#[derive(Clone, Copy)]
struct RosedaleVoiceState {
    phase: f64,
    sample_history: f64,
    valve_aperature: f64,
    opening: bool,
    attack: f64,
    freq: f64,
}

impl RosedaleVoiceState {
    fn new(note_index: u8) -> Self {
        let freq = 440.0 * 2.0_f64.powf((note_index as f64 - 69.0) / 12.0);

        Self {
            phase: 0.00,
            sample_history: 0.0,
            valve_aperature: 0.0,
            opening: false,
            attack: 0.0,
            freq: freq,
        }
    }
}

fn calc_dt(sample_rate: f64) -> f64 {
    1.0 / sample_rate
}

fn calculate_alpha(cutoff_freq: f64, dt: f64) -> f64 {
    //The filter coefficient for the plastic organ body
    let omega_dt = 2.0 * PI * cutoff_freq * dt;

    omega_dt / (1.0 + omega_dt)
}

//Dont forget to scale the midi velocity to an attack
fn update_aperature(
    voice_state: &mut RosedaleVoiceState,
    pressure: &PlenumPressure,
    params: &RosedaleParams,
    dt: f64,
) {
    let mut aperature_t = voice_state.valve_aperature;

    if voice_state.opening {
        const SLOW_OPEN: f64 = 0.08;
        const FAST_OPEN: f64 = 0.01;

        let valve_speed: f64 = 1.0 / (SLOW_OPEN - (voice_state.attack * (SLOW_OPEN - FAST_OPEN)));

        aperature_t += (valve_speed * dt);
    } else {
        let valve_speed = params.spring_speed / (1.0 + (0.875 * pressure.pressure));
        aperature_t -= (valve_speed * dt);
    }
    aperature_t = aperature_t.clamp(0.0, 1.0);
    voice_state.valve_aperature = aperature_t;
}

fn update_pressure(
    pressure: &mut PlenumPressure,
    params: &RosedaleParams,
    aperature_area: f64,
    dt: f64,
) {
    let air_in = params.refill_speed * (params.max_pressure - pressure.pressure);

    let air_out = params.valve_flow_rate * aperature_area * pressure.pressure;

    pressure.pressure += (air_in - air_out) * dt;
}

fn calc_pitch_sag(pressure: &mut PlenumPressure, params: &RosedaleParams, midi_freq: f64) -> f64 {
    let sag = 1.0 - (params.pitch_modulation_depth * (params.max_pressure - pressure.pressure));
    midi_freq * sag
}

fn synthesize_pulse_wave(voice_state: &mut RosedaleVoiceState, params: &RosedaleParams) -> f64 {
    if voice_state.phase > params.pulse_duty_cycle {
        1.0
    } else {
        -1.0
    }
}

fn apply_chassis_filter(voice_state: &mut RosedaleVoiceState, alpha: f64, sample: f64) -> f64 {
    let prev_sample = voice_state.sample_history;

    let next_sample = prev_sample + alpha * (sample - prev_sample);

    voice_state.sample_history = next_sample;

    next_sample
}

struct RosedaleEngine {
    params: RosedaleParams,
    pressure: PlenumPressure,
    voices: Vec<RosedaleVoiceState>,
    sample_rate: f64,
    active_indices: Vec<usize>,
}

impl RosedaleEngine {
    fn new(sample_rate: f64) -> Self {
        let mut voices = Vec::with_capacity(128);
        for i in 0..128 {
            voices.push(RosedaleVoiceState::new(i));
        }
        Self {
            params: RosedaleParams::default(),
            pressure: PlenumPressure { pressure: 0.0 },
            voices,
            sample_rate,
            active_indices: Vec::with_capacity(128),
        }
    }

    fn handle_midi(&mut self, msg: MidiMessage) {
        match msg {
            MidiMessage::NoteOn(_, note, vel) => {
                let idx = u8::from(note) as usize;
                let v = u8::from(vel) as f64 / 127.0;

                if v > 0.0 {
                    self.voices[idx].opening = true;
                    self.voices[idx].attack = v;

                    if !self.active_indices.contains(&idx) {
                        self.active_indices.push(idx);
                    }
                } else {
                    self.voices[idx].opening = false;
                }
            }
            MidiMessage::NoteOff(_, note, _) => {
                let idx = u8::from(note) as usize;
                self.voices[idx].opening = false;
            }
            _ => {}
        }
    }

    fn process_buffer(&mut self, buffer: &mut [f32], channels: usize) {
        let dt = 1.0 / self.sample_rate;
        let alpha = calculate_alpha(self.params.filter_cutoff, dt);
        for frame in buffer.chunks_mut(channels) {
            let mut total_aperature = 0.0;
            for &i in &self.active_indices {
                total_aperature += self.voices[i].valve_aperature;
            }
            update_pressure(&mut self.pressure, &self.params, total_aperature, dt);

            let mut mono_mix = 0.0;

            for &i in &self.active_indices {
                let voice = &mut self.voices[i];
                update_aperature(voice, &mut self.pressure, &self.params, dt);

                if voice.valve_aperature <= 0.0001 {
                    continue;
                }

                let freq = calc_pitch_sag(&mut self.pressure, &self.params, voice.freq);

                voice.phase += freq * dt;
                if voice.phase > 1.0 {
                    voice.phase -= 1.0;
                }

                let raw = synthesize_pulse_wave(voice, &self.params);
                let filtered = apply_chassis_filter(voice, alpha, raw);

                mono_mix += filtered * voice.valve_aperature * self.pressure.pressure;
            }

            mono_mix = mono_mix.tanh();
            for sample in frame {
                *sample = mono_mix as f32;
            }
        }
        let voices = &self.voices;
        self.active_indices.retain(|&i| {
            let v = &voices[i];
            v.opening || v.valve_aperature > 0.0001
        });
    }
}

struct RosedaleSynth {
    _stream: Stream,
}

impl RosedaleSynth {
    fn new(mut midi_consumer: Consumer<MidiMessage<'static>>) -> Result<Self> {
        let host = default_host();
        let device = host
            .default_output_device()
            .context("No audio output device found")?;
        let config: StreamConfig = device.default_output_config()?.into();
        let mut engine = RosedaleEngine::new(config.sample_rate.0 as f64);
        let channels = config.channels as usize;

        let stream = device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                while let Ok(msg) = midi_consumer.pop() {
                    engine.handle_midi(msg);
                }
                engine.process_buffer(data, channels);
            },
            |err| eprint!("Audio stream error: {}", err),
            None,
        )?;
        stream.play()?;
        Ok(Self { _stream: stream })
    }
}

fn connect_to_midi(
    mut producer: Producer<MidiMessage<'static>>,
) -> Result<MidiInputConnection<()>> {
    let mut midi_input = MidiInput::new("Rosedale Synth")?;

    midi_input.ignore(midir::Ignore::TimeAndActiveSense);

    let midi_processor = move |_timestamp: u64, raw_bytes: &[u8], _: &mut ()| {
        if let Ok(msg) = MidiMessage::try_from(raw_bytes) {
            let _ = producer.push(msg.to_owned());
        }
    };

    let conn = midi_input
        .create_virtual("Rosedale Port", midi_processor, ())
        .map_err(|e| anyhow::anyhow!("Error creating MIDI virtual port: {}", e))?;

    Ok(conn)
}

fn main() -> Result<()> {
    let (producer, consumer) = RingBuffer::<MidiMessage<'static>>::new(128);

    let _midi_conn = connect_to_midi(producer).context("Failed to connect to MIDI");

    let _synth = RosedaleSynth::new(consumer).context("Audio Stream failed to start");

    println!("Press Enter to exit...");
    let mut input = String::new();
    stdin().read_line(&mut input);
    Ok(())
}
