use anyhow::{Context, Result, anyhow, bail};
use cudarc::driver::{CudaContext, CudaFunction, LaunchConfig, PushKernelArg};
use cudarc::nvrtc::Ptx;
use std::sync::Arc;

use crate::art::Bitmap;
use crate::gpu::{GpuEmergenceStatistics, GpuWindowScore};

const SCORE_WORDS: usize = 12;
const SCORE_SCALE: f64 = 1_000_000.0;

pub(crate) struct CudaSearchEngine {
    context: Arc<CudaContext>,
    function: CudaFunction,
    device_name: String,
}

impl CudaSearchEngine {
    pub(crate) fn new() -> Result<Self> {
        let verified = crate::cuda_artifact::verified().map_err(anyhow::Error::msg)?;
        let context = CudaContext::new(0).context("opening CUDA device 0")?;
        let device_name = context.name().context("reading CUDA device name")?;
        let module = context
            .load_module(Ptx::from_src(verified.ptx))
            .context("loading verified CUDA PTX")?;
        let function = module
            .load_function("emergence")
            .context("loading CUDA emergence kernel")?;
        Ok(Self {
            context,
            function,
            device_name,
        })
    }

    pub(crate) fn device_name(&self) -> &str {
        &self.device_name
    }

    pub(crate) fn emergence_scores(
        &self,
        digits: &[u8],
        actual_windows: usize,
        target: &Bitmap,
        canvas_width: usize,
        canvas_height: usize,
    ) -> Result<Vec<GpuWindowScore>> {
        if actual_windows == 0 {
            return Ok(Vec::new());
        }
        let shape_pixels = target.pixels.iter().filter(|pixel| **pixel == 1).count();
        if shape_pixels == 0 {
            bail!("CUDA emergence target has no shape pixels");
        }
        let background_pixels = target.pixels.len().saturating_sub(shape_pixels);
        let actual_windows_u32 = u32::try_from(actual_windows)?;
        let canvas_width_u32 = u32::try_from(canvas_width)?;
        let canvas_height_u32 = u32::try_from(canvas_height)?;
        let target_width_u32 = u32::try_from(target.width)?;
        let target_height_u32 = u32::try_from(target.height)?;
        let shape_pixels_u32 = u32::try_from(shape_pixels)?;
        let background_pixels_u32 = u32::try_from(background_pixels)?;
        let output_words = actual_windows
            .checked_mul(SCORE_WORDS)
            .context("CUDA score staging size overflowed")?;
        let stream = self.context.default_stream();
        let digit_device = stream.clone_htod(digits).context("uploading CUDA digits")?;
        let target_device = stream
            .clone_htod(&target.pixels)
            .context("uploading CUDA target")?;
        let mut output_device = stream
            .alloc_zeros::<u32>(output_words)
            .context("allocating CUDA score output")?;
        let mut launch = stream.launch_builder(&self.function);
        launch
            .arg(&digit_device)
            .arg(&target_device)
            .arg(&canvas_width_u32)
            .arg(&canvas_height_u32)
            .arg(&target_width_u32)
            .arg(&target_height_u32)
            .arg(&actual_windows_u32)
            .arg(&shape_pixels_u32)
            .arg(&background_pixels_u32)
            .arg(&mut output_device);
        // SAFETY: Category 13, library contract. The verified kernel signature
        // exactly matches the ordered arguments above, and output_words reserves
        // twelve u32 values for every invocation admitted by actual_windows.
        unsafe { launch.launch(LaunchConfig::for_num_elems(actual_windows_u32))? };
        let output = stream
            .clone_dtoh(&output_device)
            .context("reading CUDA score output")?;
        decode_scores(&output)
    }
}

fn decode_scores(output: &[u32]) -> Result<Vec<GpuWindowScore>> {
    if output.len() % SCORE_WORDS != 0 {
        bail!("CUDA score output has an invalid word count");
    }
    output
        .chunks_exact(SCORE_WORDS)
        .map(|chunk| {
            let values: &[u32; SCORE_WORDS] = chunk
                .try_into()
                .map_err(|_| anyhow!("CUDA score output has an invalid record"))?;
            let &[
                score,
                digit,
                x,
                y,
                coverage,
                leakage,
                covered,
                total,
                leaked,
                background_total,
                _,
                _,
            ] = values;
            Ok(GpuWindowScore {
                score: f64::from(score) / SCORE_SCALE,
                digit: u8::try_from(digit)?,
                x: usize::try_from(x)?,
                y: usize::try_from(y)?,
                coverage: f64::from(coverage) / SCORE_SCALE,
                leakage: f64::from(leakage) / SCORE_SCALE,
                statistics: Some(GpuEmergenceStatistics {
                    covered,
                    total,
                    leaked,
                    background_total,
                }),
            })
        })
        .collect()
}
