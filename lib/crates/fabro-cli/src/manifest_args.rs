use fabro_api::types;

use crate::args::{PreflightArgs, RunArgs};

pub(crate) fn run_manifest_args(args: &RunArgs) -> Option<types::ManifestArgs> {
    let payload = types::ManifestArgs {
        auto_approve:     args.auto_approve.then_some(true),
        dry_run:          args.dry_run.then_some(true),
        label:            args.label.clone(),
        model:            args.model.clone(),
        preserve_sandbox: args.preserve_sandbox.then_some(true),
        provider:         args.provider.clone(),
        sandbox:          args
            .sandbox
            .map(|provider| fabro_sandbox::SandboxProvider::from(provider).to_string()),
        docker_image:     None,
        input:            args.inputs.values.clone(),
        verbose:          args.verbose.then_some(true),
    };
    (!fabro_manifest::manifest_args_is_empty(&payload)).then_some(payload)
}

pub(crate) fn preflight_manifest_args(args: &PreflightArgs) -> Option<types::ManifestArgs> {
    let payload = types::ManifestArgs {
        auto_approve:     None,
        dry_run:          None,
        label:            Vec::new(),
        model:            args.model.clone(),
        preserve_sandbox: None,
        provider:         args.provider.clone(),
        sandbox:          args
            .sandbox
            .map(|provider| fabro_sandbox::SandboxProvider::from(provider).to_string()),
        docker_image:     None,
        input:            args.inputs.values.clone(),
        verbose:          args.verbose.then_some(true),
    };
    (!fabro_manifest::manifest_args_is_empty(&payload)).then_some(payload)
}
