struct RosedaleParams {
    target_static_pressure: f64,
    plenum_refill_speed: f64,
    valve_flow_rate: f64,
    pulse_duty_cycle: f64,
    pitch_sensitivity: f64,

    tuning_aligner_variable: f64,

    chassis_absorptio_freq: f64,
}

impl Default for RosedaleParams {
    fn default() -> Self {
        RosedaleParams {
            target_static_pressure: 1.0,
            plenum_refill_speed: 10.0,
            valve_flow_rate: 0.6,
            pulse_duty_cycle: 0.3,
            pitch_sensitivity: 0.06,

            tuning_aligner_variable: 1.0,

            chassis_absorptio_freq: 1500.0,
        }
    }
}

struct RosedaleState {
    plenum_pressure: f64,
    phase_accumulator: f64,
    filter_history_sample: f64,
}

fn main() {
    println!("Hello, world!");
}
