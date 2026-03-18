macro_rules! trycmd_subcommand {
    ($name:ident, $dir:expr) => {
        #[test]
        fn $name() {
            trycmd::TestCases::new()
                .case(concat!("tests/cmd/", $dir, "/*.trycmd"))
                .case(concat!("tests/cmd/", $dir, "/*.toml"));
        }
    };
}

trycmd_subcommand!(cli_cp, "cp");
trycmd_subcommand!(cli_doctor, "doctor");
trycmd_subcommand!(cli_exec, "exec");
trycmd_subcommand!(cli_init, "init");
trycmd_subcommand!(cli_install, "install");
trycmd_subcommand!(cli_llm, "llm");
trycmd_subcommand!(cli_model, "model");
trycmd_subcommand!(cli_pr, "pr");
trycmd_subcommand!(cli_repo, "repo");
trycmd_subcommand!(cli_preview, "preview");
trycmd_subcommand!(cli_run, "run");
trycmd_subcommand!(cli_ssh, "ssh");
trycmd_subcommand!(cli_system, "system");
trycmd_subcommand!(cli_top_level, "top-level");
trycmd_subcommand!(cli_validate, "validate");
