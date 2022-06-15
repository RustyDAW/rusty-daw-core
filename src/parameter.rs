// Some modified code from baseplug:
//
// https://github.com/wrl/baseplug/blob/trunk/src/parameter.rs
// https://github.com/wrl/baseplug/blob/trunk/LICENSE-APACHE
// https://github.com/wrl/baseplug/blob/trunk/LICENSE-MIT
//
//  Thanks wrl! :)

use std::sync::{
    atomic::{AtomicI32, Ordering},
    Arc,
};

use super::atomic::{AtomicF32, AtomicF64};
use super::decibel::{
    coeff_to_db_clamped_neg_90_db_f32, coeff_to_db_clamped_neg_90_db_f64,
    db_to_coeff_clamped_neg_90_db_f32, db_to_coeff_clamped_neg_90_db_f64,
};
use super::{SampleRate, Seconds, SmoothF32, SmoothF64, SmoothOutputF32, SmoothOutputF64};

/// A good default value to use as `smooth_secs` parameter when creating a [`ParamF32`]/[`ParamF64`].
///
/// This specifies that the low-pass parameter smoothing filter should use a period of `5 ms`.
///
/// [`ParamF32`]: struct.ParamF32.html
/// [`ParamF64`]: struct.ParamF64.html
pub const DEFAULT_SMOOTH_SECS: Seconds = Seconds(5.0 / 1_000.0);

/// A good default value to use as `gradient` parameter when creating a [`ParamF32`]/[`ParamF64`] that
/// deals with decibels.
pub const DEFAULT_DB_GRADIENT: Gradient = Gradient::Power(0.15);

/// The gradient used when mapping the normalized value in the range `[0.0, 1.0]` to the
/// desired value.
///
/// For example, it is useful for parameters dealing with decibels to have a mapping
/// gradient around `Power(0.15)`. This is so one tick near the top of the slider/knob
/// controlling this parameter causes a small change in dB around `0.0 dB` and one tick
/// on the other end causes a large change in dB around `-90.0 dB`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gradient {
    /// Linear mapping
    Linear,
    /// Power mapping
    ///
    /// For example, it is useful for parameters dealing with decibels to have a mapping
    /// gradient around `Power(0.15)`. This is so one tick near the top of the slider/knob
    /// controlling this parameter causes a small change in dB around `0.0 dB` and one tick
    /// on the other end causes a large change in dB around `-90.0 dB`.
    Power(f32),
    /// Exponential (logarithmic) mapping
    ///
    /// This is useful for parameters dealing with frequency in Hz.
    Exponential,
}

/// The unit of this parameter. This signifies how the value displayed to the end user should
/// differ from the actual value used in DSP.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Unit {
    /// Any kind of unit where the value displayed to the end user is the same value used
    /// in the DSP.
    Generic,
    /// Signifies that the value displayed to the end user should be in decibels and the
    /// value used in the DSP should be in raw amplitude.
    ///
    /// In addition, whenever the dB value is less than or equal to `-90.0 dB`, then the
    /// resulting raw DSP ampilitude value will be clamped to `0.0` (essentially equaling
    /// `-infinity dB`).
    Decibels,
}

impl Unit {
    /// Convert the given unit value to the corresponding raw value used in DSP.
    ///
    /// This is only effective when this unit is not of type `Unit::Generic`.
    pub fn unit_to_dsp_f32(&self, value: f32) -> f32 {
        match self {
            Unit::Decibels => db_to_coeff_clamped_neg_90_db_f32(value),
            _ => value,
        }
    }

    /// Convert the given raw DSP value to the corresponding unit value.
    ///
    /// This is only effective when this unit is not of type `Unit::Generic`.
    pub fn dsp_to_unit_f32(&self, dsp_value: f32) -> f32 {
        match self {
            Unit::Decibels => coeff_to_db_clamped_neg_90_db_f32(dsp_value),
            _ => dsp_value,
        }
    }

    /// Convert the given unit value to the corresponding raw value used in DSP.
    ///
    /// This is only effective when this unit is not of type `Unit::Generic`.
    pub fn unit_to_dsp_f64(&self, value: f64) -> f64 {
        match self {
            Unit::Decibels => db_to_coeff_clamped_neg_90_db_f64(value),
            _ => value,
        }
    }

    /// Convert the given raw DSP value to the corresponding unit value.
    ///
    /// This is only effective when this unit is not of type `Unit::Generic`.
    pub fn dsp_to_unit_f64(&self, dsp_value: f64) -> f64 {
        match self {
            Unit::Decibels => coeff_to_db_clamped_neg_90_db_f64(dsp_value),
            _ => dsp_value,
        }
    }
}

/// An auto-smoothed parameter with an `f32` value.
pub struct ParamF32<const MAX_BLOCKSIZE: usize> {
    min: f32,
    max: f32,
    gradient: Gradient,
    unit: Unit,

    shared_normalized: Arc<AtomicF32>,
    normalized: f32,

    value: f32,

    smoothed: SmoothF32<MAX_BLOCKSIZE>,
    smooth_secs: Seconds,
}

impl<const MAX_BLOCKSIZE: usize> ParamF32<MAX_BLOCKSIZE> {
    /// Create a Parameter/Handle pair from its (de-normalized) value.
    ///
    /// * value - The initial (de-normalized) value of the parameter.
    /// * min - The minimum (de-normalized) value of the parameter.
    /// * max - The maximum (de-normalized) value of the parameter.
    /// * gradient - The [`Gradient`] mapping used when converting from the normalized value
    /// in the range `[0.0, 1.0]` to the desired value. If this parameter deals with decibels,
    /// you may use `ParamF32::DEFAULT_SMOOTH_SECS` as a good default.
    /// * unit - The [`Unit`] that signifies how the value displayed to the end user should
    /// differ from the actual value used in DSP.
    /// * smooth_secs: The period of the low-pass parameter smoothing filter (for declicking). You
    /// may use `ParamF32::DEFAULT_SMOOTH_SECS` as a good default.
    /// * sample_rate: The sample rate of this process. This is used for the low-pass parameter
    /// smoothing filter.
    ///
    /// [`Gradient`]: enum.Gradient.html
    /// [`Unit`]: enum.Unit.html
    pub fn from_value(
        value: f32,
        min: f32,
        max: f32,
        gradient: Gradient,
        unit: Unit,
        smooth_secs: Seconds,
        sample_rate: SampleRate,
    ) -> (Self, ParamF32Handle) {
        let normalized = value_to_normalized_f32(value, min, max, gradient);

        let handle_value = normalized_to_value_f32(normalized, min, max, gradient);
        let rt_value = match unit {
            Unit::Decibels => db_to_coeff_clamped_neg_90_db_f32(handle_value),
            _ => handle_value,
        };

        let shared_normalized = Arc::new(AtomicF32::new(normalized));

        let mut smoothed = SmoothF32::new(rt_value);
        smoothed.set_speed(sample_rate, smooth_secs);

        (
            Self {
                min,
                max,
                gradient,
                unit,
                shared_normalized: Arc::clone(&shared_normalized),
                normalized,
                value: rt_value,
                smoothed,
                smooth_secs,
            },
            ParamF32Handle {
                min,
                max,
                gradient,
                unit,
                shared_normalized,
            },
        )
    }

    /// Create a Parameter/Handle pair from its normalized value in the range `[0.0, 1.0]`.
    ///
    /// * value - The initial normalized value of the parameter in the range `[0.0, 1.0]`.
    /// * min - The minimum (de-normalized) value of the parameter.
    /// * max - The maximum (de-normalized) value of the parameter.
    /// * gradient - The [`Gradient`] mapping used when converting from the normalized value
    /// in the range `[0.0, 1.0]` to the desired value. If this parameter deals with decibels,
    /// you may use `ParamF32::DEFAULT_SMOOTH_SECS` as a good default.
    /// * unit - The [`Unit`] that signifies how the value displayed to the end user should
    /// differ from the actual value used in DSP.
    /// * smooth_secs: The period of the low-pass parameter smoothing filter (for declicking). You
    /// may use `ParamF32::DEFAULT_SMOOTH_SECS` as a good default.
    /// * sample_rate: The sample rate of this process. This is used for the low-pass parameter
    /// smoothing filter.
    ///
    /// [`Gradient`]: enum.Gradient.html
    /// [`Unit`]: enum.Unit.html
    pub fn from_normalized(
        normalized: f32,
        min_value: f32,
        max_value: f32,
        gradient: Gradient,
        unit: Unit,
        smooth_secs: Seconds,
        sample_rate: SampleRate,
    ) -> (Self, ParamF32Handle) {
        let normalized = normalized.clamp(0.0, 1.0);

        let shared_normalized = Arc::new(AtomicF32::new(normalized));

        let handle_value = normalized_to_value_f32(normalized, min_value, max_value, gradient);
        let rt_value = match unit {
            Unit::Decibels => db_to_coeff_clamped_neg_90_db_f32(handle_value),
            _ => handle_value,
        };

        let mut smoothed = SmoothF32::new(rt_value);
        smoothed.set_speed(sample_rate, smooth_secs);

        (
            Self {
                min: min_value,
                max: max_value,
                gradient,
                unit,
                shared_normalized: Arc::clone(&shared_normalized),
                normalized,
                value: rt_value,
                smoothed,
                smooth_secs,
            },
            ParamF32Handle {
                min: min_value,
                max: max_value,
                gradient,
                unit,
                shared_normalized,
            },
        )
    }

    /// Set the (de-normalized) value of this parameter.
    pub fn set_value(&mut self, value: f32) {
        if self.value != value {
            self.normalized = value_to_normalized_f32(value, self.min, self.max, self.gradient);
            self.shared_normalized.set(self.normalized);

            let v = normalized_to_value_f32(self.normalized, self.min, self.max, self.gradient);
            self.value = match self.unit {
                Unit::Decibels => db_to_coeff_clamped_neg_90_db_f32(v),
                _ => v,
            };

            self.smoothed.set(self.value);
        }
    }

    /// Set the normalized value of this parameter in the range `[0.0, 1.0]`.
    pub fn set_normalized(&mut self, normalized: f32) {
        if self.normalized != normalized {
            self.normalized = normalized.clamp(0.0, 1.0);
            self.shared_normalized.set(self.normalized);

            let v = normalized_to_value_f32(self.normalized, self.min, self.max, self.gradient);
            self.value = match self.unit {
                Unit::Decibels => db_to_coeff_clamped_neg_90_db_f32(v),
                _ => v,
            };

            self.smoothed.set(self.value);
        }
    }

    /// Reset this parameter (without any smoothing) to the given (de-normalized) value.
    pub fn reset_from_value(&mut self, value: f32) {
        self.normalized = value_to_normalized_f32(value, self.min, self.max, self.gradient);
        self.shared_normalized.set(self.normalized);

        let v = normalized_to_value_f32(self.normalized, self.min, self.max, self.gradient);
        self.value = match self.unit {
            Unit::Decibels => db_to_coeff_clamped_neg_90_db_f32(v),
            _ => v,
        };

        self.smoothed.reset(self.value);
    }

    /// Reset this parameter (without any smoothing) to the given normalized value in the range `[0.0, 1.0]`.
    pub fn reset_from_normalized(&mut self, normalized: f32) {
        self.normalized = normalized.clamp(0.0, 1.0);
        self.shared_normalized.set(self.normalized);

        let v = normalized_to_value_f32(self.normalized, self.min, self.max, self.gradient);
        self.value = match self.unit {
            Unit::Decibels => db_to_coeff_clamped_neg_90_db_f32(v),
            _ => v,
        };

        self.smoothed.reset(self.value);
    }

    /// Reset the internal smoothing buffer.
    pub fn reset(&mut self) {
        self.smoothed.reset(self.value);
    }

    /// Get the smoothed buffer of values for use in DSP.
    pub fn smoothed(&mut self, frames: usize) -> SmoothOutputF32<MAX_BLOCKSIZE> {
        let new_normalized = self.shared_normalized.get();
        if self.normalized != new_normalized {
            self.normalized = new_normalized;

            let v = normalized_to_value_f32(self.normalized, self.min, self.max, self.gradient);
            self.value = match self.unit {
                Unit::Decibels => db_to_coeff_clamped_neg_90_db_f32(v),
                _ => v,
            };

            self.smoothed.set(self.value);
        }

        self.smoothed.process(frames);
        self.smoothed.update_status();

        self.smoothed.output()
    }

    /// Update the sample rate (used for the parameter smoothing LPF).
    pub fn set_sample_rate(&mut self, sample_rate: SampleRate) {
        self.smoothed.set_speed(sample_rate, self.smooth_secs);
    }

    /// The minimum value of this parameter.
    pub fn min(&self) -> f32 {
        self.min
    }

    /// The maximum value of this parameter.
    pub fn max(&self) -> f32 {
        self.max
    }

    /// The [`Gradient`] mapping used when converting from the normalized value
    /// in the range `[0.0, 1.0]` to the desired value.
    ///
    /// [`Gradient`]: enum.Gradient.html
    pub fn gradient(&self) -> Gradient {
        self.gradient
    }

    /// The [`Unit`] that signifies how the value displayed to the end user should
    /// differ from the actual value used in DSP.
    ///
    /// [`Unit`]: enum.Unit.html
    pub fn unit(&self) -> Unit {
        self.unit
    }

    /// Convert the given value to the corresponding normalized range `[0.0, 1.0]`
    /// of this parameter.
    pub fn value_to_normalized(&self, value: f32) -> f32 {
        value_to_normalized_f32(value, self.min, self.max, self.gradient)
    }

    /// Convert the given normalized value in the range `[0.0, 1.0]` into the
    /// corresponding value of this parameter.
    pub fn normalized_to_value(&self, normalized: f32) -> f32 {
        normalized_to_value_f32(normalized, self.min, self.max, self.gradient)
    }

    /// The current normalized value in the range `[0.0, 1.0]`. This is only meant for
    /// communicating with the host. This is not meant to be used to retrieve the latest
    /// value for DSP. To get the latest value for DSP please use `ParamF32::smoothed()`
    /// instead.
    ///
    /// Please note that this should be called *after* calling `ParamF32::smoothed()`
    /// if you need the latest value from the corresponding [`ParamF32Handle`],
    /// otherwise this may not return the latest value.
    ///
    /// [`ParamF32Handle`]: struct.ParamF32Handle.html
    pub fn normalized(&self) -> f32 {
        self.normalized
    }

    /// Get the shared normalized float value.
    ///
    /// This can be useful to integrate with various plugin APIs.
    pub fn shared_normalized(&self) -> Arc<AtomicF32> {
        Arc::clone(&self.shared_normalized)
    }
}

/// A handle to get and update the value of an auto-smoothed [`ParamF32`] from a UI.
///
/// [`ParamF32`]: struct.ParamF32.html
pub struct ParamF32Handle {
    min: f32,
    max: f32,
    gradient: Gradient,
    unit: Unit,

    shared_normalized: Arc<AtomicF32>,
}

impl ParamF32Handle {
    /// The normalized value in the range `[0.0, 1.0]`.
    pub fn normalized(&self) -> f32 {
        self.shared_normalized.get()
    }

    /// The (un-normalized) value of this parameter.
    ///
    /// Please note that this is calculated from the shared normalized value every time, so
    /// avoid calling this every frame if you can.
    pub fn value(&self) -> f32 {
        normalized_to_value_f32(
            self.shared_normalized.get(),
            self.min,
            self.max,
            self.gradient,
        )
    }

    /// Set the normalized value of this parameter in the range `[0.0, 1.0]`.
    ///
    /// Please note that this will ***NOT*** automatically notify the host of the value change
    /// if you are using this inside a plugin spec such as VST. It is intended for you use your
    /// own method for achieving this.
    pub fn set_normalized(&self, normalized: f32) {
        self.shared_normalized.set(normalized.clamp(0.0, 1.0));
    }

    /// Set the (un-normalized) value of this parameter.
    ///
    /// Please note that this will ***NOT*** automatically notify the host of the value change
    /// if you are using this inside a plugin spec such as VST. It is intended for you use your
    /// own method for achieving this.
    pub fn set_value(&self, value: f32) {
        let normalized = value_to_normalized_f32(value, self.min, self.max, self.gradient);
        self.set_normalized(normalized);
    }

    /// The minimum value of this parameter.
    pub fn min(&self) -> f32 {
        self.min
    }

    /// The maximum value of this parameter.
    pub fn max(&self) -> f32 {
        self.max
    }

    /// The [`Gradient`] mapping used when converting from the normalized value
    /// in the range `[0.0, 1.0]` to the desired value.
    ///
    /// [`Gradient`]: enum.Gradient.html
    pub fn gradient(&self) -> Gradient {
        self.gradient
    }

    /// The [`Unit`] that signifies how the value displayed to the end user should
    /// differ from the actual value used in DSP.
    ///
    /// [`Unit`]: enum.Unit.html
    pub fn unit(&self) -> Unit {
        self.unit
    }

    /// Convert the given value to the corresponding normalized range `[0.0, 1.0]`
    /// of this parameter.
    pub fn value_to_normalized(&self, value: f32) -> f32 {
        value_to_normalized_f32(value, self.min, self.max, self.gradient)
    }

    /// Convert the given normalized value in the range `[0.0, 1.0]` into the
    /// corresponding value of this parameter.
    pub fn normalized_to_value(&self, normalized: f32) -> f32 {
        normalized_to_value_f32(normalized, self.min, self.max, self.gradient)
    }

    /// Get the shared normalized float value.
    ///
    /// This can be useful to integrate with various plugin APIs.
    pub fn shared_normalized(&self) -> Arc<AtomicF32> {
        Arc::clone(&self.shared_normalized)
    }
}

impl Clone for ParamF32Handle {
    fn clone(&self) -> Self {
        Self {
            min: self.min,
            max: self.max,
            gradient: self.gradient,
            unit: self.unit,

            shared_normalized: Arc::clone(&self.shared_normalized),
        }
    }
}

fn normalized_to_value_f32(normalized: f32, min: f32, max: f32, gradient: Gradient) -> f32 {
    let normalized = normalized.clamp(0.0, 1.0);

    let map = |x: f32| -> f32 {
        let range = max - min;
        (x * range) + min
    };

    match gradient {
        Gradient::Linear => map(normalized),

        Gradient::Power(exponent) => map(normalized.powf(exponent)),

        Gradient::Exponential => {
            if normalized == 0.0 {
                return min;
            }

            if normalized == 1.0 {
                return max;
            }

            let minl = min.log2();
            let range = max.log2() - minl;
            2.0f32.powf((normalized * range) + minl)
        }
    }
}

fn value_to_normalized_f32(value: f32, min: f32, max: f32, gradient: Gradient) -> f32 {
    if value <= min {
        return 0.0;
    }

    if value >= max {
        return 1.0;
    }

    let unmap = |x: f32| -> f32 {
        let range = max - min;
        (x - min) / range
    };

    match gradient {
        Gradient::Linear => unmap(value),

        Gradient::Power(exponent) => unmap(value).powf(1.0 / exponent),

        Gradient::Exponential => {
            let minl = min.log2();
            let range = max.log2() - minl;
            (value.log2() - minl) / range
        }
    }
}

// ------  F64  -------------------------------------------------------------------------

/// An auto-smoothed parameter with an `f64` value.
pub struct ParamF64<const MAX_BLOCKSIZE: usize> {
    min: f64,
    max: f64,
    gradient: Gradient,
    unit: Unit,

    shared_normalized: Arc<AtomicF64>,
    normalized: f64,

    value: f64,

    smoothed: SmoothF64<MAX_BLOCKSIZE>,
    smooth_secs: Seconds,
}

impl<const MAX_BLOCKSIZE: usize> ParamF64<MAX_BLOCKSIZE> {
    /// Create a Parameter/Handle pair from its (de-normalized) value.
    ///
    /// * value - The initial (de-normalized) value of the parameter.
    /// * min - The minimum (de-normalized) value of the parameter.
    /// * max - The maximum (de-normalized) value of the parameter.
    /// * gradient - The [`Gradient`] mapping used when converting from the normalized value
    /// in the range `[0.0, 1.0]` to the desired value. If this parameter deals with decibels,
    /// you may use `ParamF64::DEFAULT_SMOOTH_SECS` as a good default.
    /// * unit - The [`Unit`] that signifies how the value displayed to the end user should
    /// differ from the actual value used in DSP.
    /// * smooth_secs: The period of the low-pass parameter smoothing filter (for declicking). You
    /// may use `ParamF64::DEFAULT_SMOOTH_SECS` as a good default.
    /// * sample_rate: The sample rate of this process. This is used for the low-pass parameter
    /// smoothing filter.
    ///
    /// [`Gradient`]: enum.Gradient.html
    /// [`Unit`]: enum.Unit.html
    pub fn from_value(
        value: f64,
        min: f64,
        max: f64,
        gradient: Gradient,
        unit: Unit,
        smooth_secs: Seconds,
        sample_rate: SampleRate,
    ) -> (Self, ParamF64Handle) {
        let normalized = value_to_normalized_f64(value, min, max, gradient);

        let handle_value = normalized_to_value_f64(normalized, min, max, gradient);
        let rt_value = match unit {
            Unit::Decibels => db_to_coeff_clamped_neg_90_db_f64(handle_value),
            _ => handle_value,
        };

        let shared_normalized = Arc::new(AtomicF64::new(normalized));

        let mut smoothed = SmoothF64::new(rt_value);
        smoothed.set_speed(sample_rate, smooth_secs);

        (
            Self {
                min,
                max,
                gradient,
                unit,
                shared_normalized: Arc::clone(&shared_normalized),
                normalized,
                value: rt_value,
                smoothed,
                smooth_secs,
            },
            ParamF64Handle {
                min,
                max,
                gradient,
                unit,
                shared_normalized,
            },
        )
    }

    /// Create a Parameter/Handle pair from its normalized value in the range `[0.0, 1.0]`.
    ///
    /// * value - The initial normalized value of the parameter in the range `[0.0, 1.0]`.
    /// * min - The minimum (de-normalized) value of the parameter.
    /// * max - The maximum (de-normalized) value of the parameter.
    /// * gradient - The [`Gradient`] mapping used when converting from the normalized value
    /// in the range `[0.0, 1.0]` to the desired value. If this parameter deals with decibels,
    /// you may use `ParamF64::DEFAULT_SMOOTH_SECS` as a good default.
    /// * unit - The [`Unit`] that signifies how the value displayed to the end user should
    /// differ from the actual value used in DSP.
    /// * smooth_secs: The period of the low-pass parameter smoothing filter (for declicking). You
    /// may use `ParamF64::DEFAULT_SMOOTH_SECS` as a good default.
    /// * sample_rate: The sample rate of this process. This is used for the low-pass parameter
    /// smoothing filter.
    ///
    /// [`Gradient`]: enum.Gradient.html
    /// [`Unit`]: enum.Unit.html
    pub fn from_normalized(
        normalized: f64,
        min_value: f64,
        max_value: f64,
        gradient: Gradient,
        unit: Unit,
        smooth_secs: Seconds,
        sample_rate: SampleRate,
    ) -> (Self, ParamF64Handle) {
        let normalized = normalized.clamp(0.0, 1.0);

        let shared_normalized = Arc::new(AtomicF64::new(normalized));

        let handle_value = normalized_to_value_f64(normalized, min_value, max_value, gradient);
        let rt_value = match unit {
            Unit::Decibels => db_to_coeff_clamped_neg_90_db_f64(handle_value),
            _ => handle_value,
        };

        let mut smoothed = SmoothF64::new(rt_value);
        smoothed.set_speed(sample_rate, smooth_secs);

        (
            Self {
                min: min_value,
                max: max_value,
                gradient,
                unit,
                shared_normalized: Arc::clone(&shared_normalized),
                normalized,
                value: rt_value,
                smoothed,
                smooth_secs,
            },
            ParamF64Handle {
                min: min_value,
                max: max_value,
                gradient,
                unit,
                shared_normalized,
            },
        )
    }

    /// Set the (de-normalized) value of this parameter.
    pub fn set_value(&mut self, value: f64) {
        if self.value != value {
            self.normalized = value_to_normalized_f64(value, self.min, self.max, self.gradient);
            self.shared_normalized.set(self.normalized);

            let v = normalized_to_value_f64(self.normalized, self.min, self.max, self.gradient);
            self.value = match self.unit {
                Unit::Decibels => db_to_coeff_clamped_neg_90_db_f64(v),
                _ => v,
            };

            self.smoothed.set(self.value);
        }
    }

    /// Set the normalized value of this parameter in the range `[0.0, 1.0]`.
    pub fn set_normalized(&mut self, normalized: f64) {
        if self.normalized != normalized {
            self.normalized = normalized.clamp(0.0, 1.0);
            self.shared_normalized.set(self.normalized);

            let v = normalized_to_value_f64(self.normalized, self.min, self.max, self.gradient);
            self.value = match self.unit {
                Unit::Decibels => db_to_coeff_clamped_neg_90_db_f64(v),
                _ => v,
            };

            self.smoothed.set(self.value);
        }
    }

    /// Reset this parameter (without any smoothing) to the given (de-normalized) value.
    pub fn reset_from_value(&mut self, value: f64) {
        self.normalized = value_to_normalized_f64(value, self.min, self.max, self.gradient);
        self.shared_normalized.set(self.normalized);

        let v = normalized_to_value_f64(self.normalized, self.min, self.max, self.gradient);
        self.value = match self.unit {
            Unit::Decibels => db_to_coeff_clamped_neg_90_db_f64(v),
            _ => v,
        };

        self.smoothed.reset(self.value);
    }

    /// Reset this parameter (without any smoothing) to the given normalized value in the range `[0.0, 1.0]`.
    pub fn reset_from_normalized(&mut self, normalized: f64) {
        self.normalized = normalized.clamp(0.0, 1.0);
        self.shared_normalized.set(self.normalized);

        let v = normalized_to_value_f64(self.normalized, self.min, self.max, self.gradient);
        self.value = match self.unit {
            Unit::Decibels => db_to_coeff_clamped_neg_90_db_f64(v),
            _ => v,
        };

        self.smoothed.reset(self.value);
    }

    /// Reset the internal smoothing buffer.
    pub fn reset(&mut self) {
        self.smoothed.reset(self.value);
    }

    /// Get the smoothed buffer of values for use in DSP.
    pub fn smoothed(&mut self, frames: usize) -> SmoothOutputF64<MAX_BLOCKSIZE> {
        let new_normalized = self.shared_normalized.get();
        if self.normalized != new_normalized {
            self.normalized = new_normalized;

            let v = normalized_to_value_f64(self.normalized, self.min, self.max, self.gradient);
            self.value = match self.unit {
                Unit::Decibels => db_to_coeff_clamped_neg_90_db_f64(v),
                _ => v,
            };

            self.smoothed.set(self.value);
        }

        self.smoothed.process(frames);
        self.smoothed.update_status();

        self.smoothed.output()
    }

    /// Update the sample rate (used for the parameter smoothing LPF).
    pub fn set_sample_rate(&mut self, sample_rate: SampleRate) {
        self.smoothed.set_speed(sample_rate, self.smooth_secs);
    }

    /// The minimum value of this parameter.
    pub fn min(&self) -> f64 {
        self.min
    }

    /// The maximum value of this parameter.
    pub fn max(&self) -> f64 {
        self.max
    }

    /// The [`Gradient`] mapping used when converting from the normalized value
    /// in the range `[0.0, 1.0]` to the desired value.
    ///
    /// [`Gradient`]: enum.Gradient.html
    pub fn gradient(&self) -> Gradient {
        self.gradient
    }

    /// The [`Unit`] that signifies how the value displayed to the end user should
    /// differ from the actual value used in DSP.
    ///
    /// [`Unit`]: enum.Unit.html
    pub fn unit(&self) -> Unit {
        self.unit
    }

    /// Convert the given value to the corresponding normalized range `[0.0, 1.0]`
    /// of this parameter.
    pub fn value_to_normalized(&self, value: f64) -> f64 {
        value_to_normalized_f64(value, self.min, self.max, self.gradient)
    }

    /// Convert the given normalized value in the range `[0.0, 1.0]` into the
    /// corresponding value of this parameter.
    pub fn normalized_to_value(&self, normalized: f64) -> f64 {
        normalized_to_value_f64(normalized, self.min, self.max, self.gradient)
    }

    /// The current normalized value in the range `[0.0, 1.0]`. This is only meant for
    /// communicating with the host. This is not meant to be used to retrieve the latest
    /// value for DSP. To get the latest value for DSP please use `ParamF64::smoothed()`
    /// instead.
    ///
    /// Please note that this should be called *after* calling `ParamF64::smoothed()`
    /// if you need the latest value from the corresponding [`ParamF64Handle`],
    /// otherwise this may not return the latest value.
    ///
    /// [`ParamF64Handle`]: struct.ParamF64Handle.html
    pub fn normalized(&self) -> f64 {
        self.normalized
    }

    /// Get the shared normalized float value.
    ///
    /// This can be useful to integrate with various plugin APIs.
    pub fn shared_normalized(&self) -> Arc<AtomicF64> {
        Arc::clone(&self.shared_normalized)
    }
}

/// A handle to get and update the value of an auto-smoothed [`ParamF64`] from a UI.
///
/// [`ParamF64`]: struct.ParamF64.html
pub struct ParamF64Handle {
    min: f64,
    max: f64,
    gradient: Gradient,
    unit: Unit,

    shared_normalized: Arc<AtomicF64>,
}

impl ParamF64Handle {
    /// The normalized value in the range `[0.0, 1.0]`.
    pub fn normalized(&self) -> f64 {
        self.shared_normalized.get()
    }

    /// The (un-normalized) value of this parameter.
    ///
    /// Please note that this is calculated from the shared normalized value every time, so
    /// avoid calling this every frame if you can.
    pub fn value(&self) -> f64 {
        normalized_to_value_f64(
            self.shared_normalized.get(),
            self.min,
            self.max,
            self.gradient,
        )
    }

    /// Set the normalized value of this parameter in the range `[0.0, 1.0]`.
    ///
    /// Please note that this will ***NOT*** automatically notify the host of the value change
    /// if you are using this inside a plugin spec such as VST. It is intended for you use your
    /// own method for achieving this.
    pub fn set_normalized(&self, normalized: f64) {
        self.shared_normalized.set(normalized.clamp(0.0, 1.0));
    }

    /// Set the (un-normalized) value of this parameter.
    ///
    /// Please note that this will ***NOT*** automatically notify the host of the value change
    /// if you are using this inside a plugin spec such as VST. It is intended for you use your
    /// own method for achieving this.
    pub fn set_value(&self, value: f64) {
        let normalized = value_to_normalized_f64(value, self.min, self.max, self.gradient);
        self.set_normalized(normalized);
    }

    /// The minimum value of this parameter.
    pub fn min(&self) -> f64 {
        self.min
    }

    /// The maximum value of this parameter.
    pub fn max(&self) -> f64 {
        self.max
    }

    /// The [`Gradient`] mapping used when converting from the normalized value
    /// in the range `[0.0, 1.0]` to the desired value.
    ///
    /// [`Gradient`]: enum.Gradient.html
    pub fn gradient(&self) -> Gradient {
        self.gradient
    }

    /// The [`Unit`] that signifies how the value displayed to the end user should
    /// differ from the actual value used in DSP.
    ///
    /// [`Unit`]: enum.Unit.html
    pub fn unit(&self) -> Unit {
        self.unit
    }

    /// Convert the given value to the corresponding normalized range `[0.0, 1.0]`
    /// of this parameter.
    pub fn value_to_normalized(&self, value: f64) -> f64 {
        value_to_normalized_f64(value, self.min, self.max, self.gradient)
    }

    /// Convert the given normalized value in the range `[0.0, 1.0]` into the
    /// corresponding value of this parameter.
    pub fn normalized_to_value(&self, normalized: f64) -> f64 {
        normalized_to_value_f64(normalized, self.min, self.max, self.gradient)
    }

    /// Get the shared normalized float value.
    ///
    /// This can be useful to integrate with various plugin APIs.
    pub fn shared_normalized(&self) -> Arc<AtomicF64> {
        Arc::clone(&self.shared_normalized)
    }
}

impl Clone for ParamF64Handle {
    fn clone(&self) -> Self {
        Self {
            min: self.min,
            max: self.max,
            gradient: self.gradient,
            unit: self.unit,

            shared_normalized: Arc::clone(&self.shared_normalized),
        }
    }
}

fn normalized_to_value_f64(normalized: f64, min: f64, max: f64, gradient: Gradient) -> f64 {
    let normalized = normalized.clamp(0.0, 1.0);

    let map = |x: f64| -> f64 {
        let range = max - min;
        (x * range) + min
    };

    match gradient {
        Gradient::Linear => map(normalized),

        Gradient::Power(exponent) => map(normalized.powf(f64::from(exponent))),

        Gradient::Exponential => {
            if normalized == 0.0 {
                return min;
            }

            if normalized == 1.0 {
                return max;
            }

            let minl = min.log2();
            let range = max.log2() - minl;
            2.0f64.powf((normalized * range) + minl)
        }
    }
}

fn value_to_normalized_f64(value: f64, min: f64, max: f64, gradient: Gradient) -> f64 {
    if value <= min {
        return 0.0;
    }

    if value >= max {
        return 1.0;
    }

    let unmap = |x: f64| -> f64 {
        let range = max - min;
        (x - min) / range
    };

    match gradient {
        Gradient::Linear => unmap(value),

        Gradient::Power(exponent) => unmap(value).powf(1.0 / f64::from(exponent)),

        Gradient::Exponential => {
            let minl = min.log2();
            let range = max.log2() - minl;
            (value.log2() - minl) / range
        }
    }
}

/// A parameter with an `i32` value.
pub struct ParamI32 {
    min: i32,
    max: i32,

    shared: Arc<AtomicI32>,
}

impl ParamI32 {
    /// Create a Parameter/Handle pair from its (de-normalized) value.
    ///
    /// * value - The initial (de-normalized) value of the parameter.
    /// * min - The minimum (de-normalized) value of the parameter.
    /// * max - The maximum (de-normalized) value of the parameter.
    pub fn from_value(value: i32, min: i32, max: i32) -> (Self, ParamI32Handle) {
        let value = value.clamp(min, max);

        let shared = Arc::new(AtomicI32::new(value));

        (
            Self {
                min,
                max,
                shared: Arc::clone(&shared),
            },
            ParamI32Handle { min, max, shared },
        )
    }

    /// Create a Parameter/Handle pair from its normalized value in the range `[0.0, 1.0]`.
    ///
    /// * value - The initial normalized value of the parameter in the range `[0.0, 1.0]`.
    /// * min - The minimum (de-normalized) value of the parameter.
    /// * max - The maximum (de-normalized) value of the parameter.
    pub fn from_normalized(
        normalized: f32,
        min_value: i32,
        max_value: i32,
    ) -> (Self, ParamI32Handle) {
        let normalized = normalized.clamp(0.0, 1.0);
        let value = ((normalized * (max_value as f32 - min_value as f32)) + min_value as f32)
            .round() as i32;

        Self::from_value(value, min_value, max_value)
    }

    /// Set the (de-normalized) value of this parameter.
    pub fn set_value(&mut self, value: i32) {
        self.shared
            .store(value.clamp(self.min, self.max), Ordering::Relaxed);
    }

    /// Set the normalized value of this parameter in the range `[0.0, 1.0]`.
    pub fn set_normalized(&mut self, normalized: f32) {
        let normalized = normalized.clamp(0.0, 1.0);
        let value =
            ((normalized * (self.max as f32 - self.min as f32)) + self.max as f32).round() as i32;

        self.set_value(value);
    }

    /// The (un-normalized) value of this parameter.
    pub fn value(&mut self) -> i32 {
        self.shared.load(Ordering::Relaxed)
    }

    /// The minimum value of this parameter.
    pub fn min(&self) -> i32 {
        self.min
    }

    /// The maximum value of this parameter.
    pub fn max(&self) -> i32 {
        self.max
    }

    /// Convert the given value to the corresponding normalized range `[0.0, 1.0]`
    /// of this parameter.
    pub fn value_to_normalized(&self, value: i32) -> f32 {
        let value = value.clamp(self.min, self.max);
        (value - self.min) as f32 / (self.max as f32 - self.min as f32)
    }

    /// Convert the given normalized value in the range `[0.0, 1.0]` into the
    /// corresponding value of this parameter.
    pub fn normalized_to_value(&self, normalized: f32) -> i32 {
        let normalized = normalized.clamp(0.0, 1.0);
        ((normalized * (self.max as f32 - self.min as f32)) + self.max as f32).round() as i32
    }
}

/// A handle to get and update the value of an auto-smoothed [`ParamF32`] from a UI.
///
/// [`ParamF32`]: struct.ParamF32.html
pub struct ParamI32Handle {
    min: i32,
    max: i32,

    shared: Arc<AtomicI32>,
}

impl ParamI32Handle {
    /// The (un-normalized) value of this parameter.
    pub fn value(&self) -> i32 {
        self.shared.load(Ordering::Relaxed)
    }

    /// Set the (de-normalized) value of this parameter.
    pub fn set_value(&mut self, value: i32) {
        self.shared
            .store(value.clamp(self.min, self.max), Ordering::Relaxed);
    }

    /// Set the normalized value of this parameter in the range `[0.0, 1.0]`.
    pub fn set_normalized(&mut self, normalized: f32) {
        let normalized = normalized.clamp(0.0, 1.0);
        let value =
            ((normalized * (self.max as f32 - self.min as f32)) + self.max as f32).round() as i32;

        self.set_value(value);
    }

    /// The minimum value of this parameter.
    pub fn min(&self) -> i32 {
        self.min
    }

    /// The maximum value of this parameter.
    pub fn max(&self) -> i32 {
        self.max
    }

    /// Convert the given value to the corresponding normalized range `[0.0, 1.0]`
    /// of this parameter.
    pub fn value_to_normalized(&self, value: i32) -> f32 {
        let value = value.clamp(self.min, self.max);
        (value - self.min) as f32 / (self.max as f32 - self.min as f32)
    }

    /// Convert the given normalized value in the range `[0.0, 1.0]` into the
    /// corresponding value of this parameter.
    pub fn normalized_to_value(&self, normalized: f32) -> i32 {
        let normalized = normalized.clamp(0.0, 1.0);
        ((normalized * (self.max as f32 - self.min as f32)) + self.max as f32).round() as i32
    }
}

impl Clone for ParamI32Handle {
    fn clone(&self) -> Self {
        Self {
            min: self.min,
            max: self.max,
            shared: Arc::clone(&self.shared),
        }
    }
}
