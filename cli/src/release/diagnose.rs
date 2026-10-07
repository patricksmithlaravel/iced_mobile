//! `icm diagnose altool|notarytool|play <file|->` (design §6, §11): reads
//! the output of a store tool the owner ran (`upload.sh` saves it next to
//! the artifacts) and maps it to catalogue ids, so an agent can act on a
//! rejected upload without trusting the tool's exit code.
//!
//! The parsers belong to the pipelines that print those commands:
//! [`super::ios::diagnose`] (altool), [`super::macos::diagnose`]
//! (notarytool), [`super::android::diagnose`] (Google Play).

use crate::catalogue::CheckId;
use crate::cli::{DiagnoseArgs, DiagnoseTool};
use crate::context::Ctx;
use crate::error::{Evidence, IcmError, Result};
use std::io::Read;

/// Runs `icm diagnose`.
pub fn run(ctx: &mut Ctx, args: &DiagnoseArgs) -> Result<()> {
    let (text, source) = if args.file == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|error| {
                IcmError::new(CheckId::UsageBadArgs, format!("cannot read stdin: {error}"))
            })?;
        (text, None)
    } else {
        let path = std::path::PathBuf::from(&args.file);
        let text = std::fs::read_to_string(&path).map_err(|error| {
            IcmError::new(
                CheckId::UsageBadArgs,
                format!("cannot read {}: {error}", crate::paths::display(&path)),
            )
        })?;
        (text, Some(path))
    };
    let input = Input {
        text,
        evidence: source.as_deref().map(Evidence::file),
    };
    match args.tool {
        DiagnoseTool::Altool => super::ios::diagnose(ctx, &input),
        DiagnoseTool::Notarytool => super::macos::diagnose(ctx, &input),
        DiagnoseTool::Play => super::android::diagnose(ctx, &input),
    }
}

/// What a parser reads.
pub struct Input {
    /// The tool's output.
    pub text: String,
    /// The file it came from (none for stdin).
    pub evidence: Option<Evidence>,
}
