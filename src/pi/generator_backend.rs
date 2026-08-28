use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::performance::GeneratorBackendChoice;

use super::generator_discovery::{
    current_executable_sha256, discover_y_cruncher, forced_variant, test_y_cruncher_path,
};
use super::y_cruncher::{ValidatedYCruncher, YCruncherFailure};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GeneratorVariant {
    Chudnovsky,
    Spigot,
    YCruncher,
}

impl GeneratorVariant {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Chudnovsky => "chudnovsky-rug-binary-split",
            Self::Spigot => "spigot-persistent",
            Self::YCruncher => "y-cruncher-external",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        match value {
            "chudnovsky-rug-binary-split" => Some(Self::Chudnovsky),
            "spigot-persistent" => Some(Self::Spigot),
            "y-cruncher-external" => Some(Self::YCruncher),
            _ => None,
        }
    }
}

#[cfg(target_env = "msvc")]
const DEFAULT_BUILTIN_VARIANT: GeneratorVariant = GeneratorVariant::Spigot;

#[cfg(not(target_env = "msvc"))]
const DEFAULT_BUILTIN_VARIANT: GeneratorVariant = GeneratorVariant::Chudnovsky;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct UnavailableGenerator {
    pub backend: String,
    pub variant: String,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub(crate) struct GeneratorSelection {
    pub requested_backend: &'static str,
    pub selected_backend: &'static str,
    pub selected_variant: Option<GeneratorVariant>,
    pub executable: Option<ValidatedYCruncher>,
    pub executable_sha256: String,
    pub y_cruncher_path_present: bool,
    pub y_cruncher_executable_sha256: String,
    pub unavailable_backends: Vec<UnavailableGenerator>,
    pub fallback: bool,
    pub fallback_reason: String,
    pub reason: String,
    variant_forced: bool,
}

impl GeneratorSelection {
    pub(crate) fn is_available(&self) -> bool {
        !self.selected_backend.is_empty()
    }

    pub(crate) fn effective_backend(&self) -> &'static str {
        match self.selected_variant {
            Some(GeneratorVariant::YCruncher) => "y-cruncher-external",
            Some(GeneratorVariant::Chudnovsky | GeneratorVariant::Spigot) => {
                if self.variant_forced {
                    self.selected_variant
                        .map_or("cpu", GeneratorVariant::as_str)
                } else {
                    "cpu"
                }
            }
            None if self.requested_backend == "y-cruncher" => "y-cruncher-external",
            None => "cpu",
        }
    }

    pub(crate) fn fallback_after_failure(&self, reason: &str) -> Result<Self> {
        let failure = YCruncherFailure::from_reason(reason);
        let mut selection =
            builtin_selection(GeneratorBackendChoice::Auto, None, DEFAULT_BUILTIN_VARIANT)?;
        selection.y_cruncher_path_present = self.y_cruncher_path_present;
        selection.y_cruncher_executable_sha256 = self.y_cruncher_executable_sha256.clone();
        selection.executable_sha256.clear();
        selection.fallback = true;
        selection.fallback_reason = failure.as_str().to_string();
        selection.variant_forced = false;
        selection.unavailable_backends.push(unavailable(failure));
        Ok(selection)
    }

    pub(crate) fn unavailable_after_failure(&self, reason: &str) -> Self {
        let failure = YCruncherFailure::from_reason(reason);
        let mut selection = self.clone();
        selection.selected_backend = "";
        selection.selected_variant = Some(GeneratorVariant::YCruncher);
        selection.executable = None;
        selection.executable_sha256.clear();
        selection.reason = failure.as_str().to_string();
        selection.unavailable_backends = vec![unavailable(failure)];
        selection
    }
}

pub(crate) fn resolve_generator(
    requested: GeneratorBackendChoice,
    preferred: Option<&Path>,
) -> Result<GeneratorSelection> {
    let test_preferred = test_y_cruncher_path();
    let preferred = preferred.or(test_preferred.as_deref());
    let forced = forced_variant()?;
    if let Some(variant) = forced {
        let mut selection = resolve_forced(requested, preferred, variant)?;
        selection.variant_forced = true;
        return Ok(selection);
    }
    match requested {
        GeneratorBackendChoice::Cpu => {
            builtin_selection(requested, preferred, DEFAULT_BUILTIN_VARIANT)
        }
        GeneratorBackendChoice::YCruncher => resolve_explicit_external(preferred),
        GeneratorBackendChoice::Auto => resolve_auto(preferred),
    }
}

fn resolve_forced(
    requested: GeneratorBackendChoice,
    preferred: Option<&Path>,
    variant: GeneratorVariant,
) -> Result<GeneratorSelection> {
    match variant {
        GeneratorVariant::Chudnovsky | GeneratorVariant::Spigot => {
            builtin_selection(requested, preferred, variant)
        }
        GeneratorVariant::YCruncher => match preferred.map(ValidatedYCruncher::parse) {
            Some(Ok(executable)) => Ok(external_selection(
                requested.as_str(),
                executable,
                false,
                "",
            )),
            Some(Err(failure)) => Ok(unavailable_selection(requested.as_str(), failure)),
            None => Ok(unavailable_selection(
                requested.as_str(),
                YCruncherFailure::ExecutableMissing,
            )),
        },
    }
}

fn resolve_explicit_external(preferred: Option<&Path>) -> Result<GeneratorSelection> {
    let candidate = match preferred {
        Some(path) => ValidatedYCruncher::parse(path),
        None => discover_y_cruncher().ok_or(YCruncherFailure::ExecutableMissing),
    };
    Ok(match candidate {
        Ok(executable) => external_selection("y-cruncher", executable, false, ""),
        Err(failure) => unavailable_selection("y-cruncher", failure),
    })
}

fn resolve_auto(preferred: Option<&Path>) -> Result<GeneratorSelection> {
    let preferred_result = preferred.map(ValidatedYCruncher::parse);
    if let Some(Ok(executable)) = preferred_result.as_ref() {
        return Ok(external_selection("auto", executable.clone(), false, ""));
    }
    if let Some(executable) = discover_y_cruncher() {
        return Ok(external_selection("auto", executable, false, ""));
    }
    let failure = preferred_result
        .and_then(Result::err)
        .unwrap_or(YCruncherFailure::ExecutableMissing);
    let mut selection =
        builtin_selection(GeneratorBackendChoice::Auto, None, DEFAULT_BUILTIN_VARIANT)?;
    selection.fallback = true;
    selection.fallback_reason = failure.as_str().to_string();
    selection.unavailable_backends.push(unavailable(failure));
    selection.executable_sha256.clear();
    Ok(selection)
}

fn builtin_selection(
    requested: GeneratorBackendChoice,
    preferred: Option<&Path>,
    variant: GeneratorVariant,
) -> Result<GeneratorSelection> {
    let valid_external = preferred.and_then(|path| ValidatedYCruncher::parse(path).ok());
    let y_hash = valid_external
        .as_ref()
        .map_or_else(String::new, |executable| executable.sha256().to_string());
    Ok(GeneratorSelection {
        requested_backend: requested.as_str(),
        selected_backend: "cpu",
        selected_variant: Some(variant),
        executable: None,
        executable_sha256: current_executable_sha256()?,
        y_cruncher_path_present: valid_external.is_some(),
        y_cruncher_executable_sha256: y_hash,
        unavailable_backends: Vec::new(),
        fallback: false,
        fallback_reason: String::new(),
        reason: String::new(),
        variant_forced: false,
    })
}

fn external_selection(
    requested_backend: &'static str,
    executable: ValidatedYCruncher,
    fallback: bool,
    fallback_reason: &str,
) -> GeneratorSelection {
    let hash = executable.sha256().to_string();
    GeneratorSelection {
        requested_backend,
        selected_backend: "y-cruncher",
        selected_variant: Some(GeneratorVariant::YCruncher),
        executable: Some(executable),
        executable_sha256: hash.clone(),
        y_cruncher_path_present: true,
        y_cruncher_executable_sha256: hash,
        unavailable_backends: Vec::new(),
        fallback,
        fallback_reason: fallback_reason.to_string(),
        reason: String::new(),
        variant_forced: false,
    }
}

fn unavailable_selection(
    requested_backend: &'static str,
    failure: YCruncherFailure,
) -> GeneratorSelection {
    GeneratorSelection {
        requested_backend,
        selected_backend: "",
        selected_variant: Some(GeneratorVariant::YCruncher),
        executable: None,
        executable_sha256: String::new(),
        y_cruncher_path_present: false,
        y_cruncher_executable_sha256: String::new(),
        unavailable_backends: vec![unavailable(failure)],
        fallback: false,
        fallback_reason: String::new(),
        reason: failure.as_str().to_string(),
        variant_forced: false,
    }
}

fn unavailable(failure: YCruncherFailure) -> UnavailableGenerator {
    unavailable_reason(failure.as_str())
}

fn unavailable_reason(reason: &str) -> UnavailableGenerator {
    UnavailableGenerator {
        backend: "y-cruncher".to_string(),
        variant: GeneratorVariant::YCruncher.as_str().to_string(),
        reason: reason.to_string(),
    }
}
