//! The owner's commands for every release target (design §11). **This is
//! the only file in `cli/src` that may contain upload, publish or
//! notarize argv**; a test below scans the rest of the source for them
//! (design §17 item 7). icm writes these commands into `UPLOAD.md` and
//! `upload.sh` and never runs them.
//!
//! Secrets appear only as environment-variable references or keychain
//! profile names. Every variable a command reads is declared with
//! [`OwnerPlan::need`], so `upload.sh` checks it first (exit 9).

use super::upload::{Command, OwnerPlan, OwnerStep, Part, StepKind, Word};
use crate::cli::ReleaseTarget;
use crate::config::{IcmToml, WebHost};
use std::path::Path;

/// What every owner plan knows about the release.
pub struct Common<'a> {
    /// The target.
    pub target: ReleaseTarget,
    /// icm.toml.
    pub config: &'a IcmToml,
    /// The Cargo version.
    pub version: &'a str,
    /// `[app] build`.
    pub build: u64,
    /// No upload of this target is in the ledger yet.
    pub first_upload: bool,
    /// The project's icm.toml (absolute), so `icm` finds the project from
    /// anywhere.
    pub icm_toml: &'a Path,
}

impl Common<'_> {
    fn config_arg(&self) -> [Word; 2] {
        [
            Word::lit("--config"),
            Word::lit(self.icm_toml.display().to_string()),
        ]
    }
}

fn lit(text: &str) -> Word {
    Word::lit(text)
}

fn cmd(words: Vec<Word>) -> Command {
    Command::new(words)
}

/// The last step of every plan: `icm ledger mark-uploaded`, so the next
/// release refuses a build number that is not higher.
pub fn mark_uploaded(c: &Common<'_>) -> OwnerStep {
    let mut words = vec![
        lit("icm"),
        lit("ledger"),
        lit("mark-uploaded"),
        lit(c.target.as_str()),
        lit("--build"),
        lit(&c.build.to_string()),
    ];
    words.extend(c.config_arg());
    OwnerStep::run(StepKind::After, "Record the upload in the ledger", cmd(words)).note(
        "Writes .icm/ledger.toml in the project; commit it. The next release then needs a higher [app] build.",
    )
}

/// A listing step's note; UPLOAD.md's Listing section adds the URLs from
/// `[store]`.
fn listing_note(c: &Common<'_>, what: &str) -> String {
    let urls = if c.target == ReleaseTarget::Ios {
        "the privacy policy and support URLs"
    } else {
        "the privacy policy URL"
    };
    format!("{what} Also {urls} (see Listing below).")
}

/// App Store Connect (design §11.1): validate, upload, confirm the build
/// landed in the right app, record it.
pub fn ios(c: &Common<'_>, ipa: &str) -> OwnerPlan {
    let config = c.config;
    let key = config.store.asc_key_id_env.as_str();
    let issuer = config.store.asc_issuer_id_env.as_str();
    let mut plan = OwnerPlan::new("App Store Connect");
    plan.need(
        key,
        "the App Store Connect API key id (Users and Access > Integrations > App Store Connect API)",
    );
    plan.need(issuer, "the App Store Connect API issuer id");

    plan.push(OwnerStep::manual(
        StepKind::Once,
        "Create the App ID, the App Store profile and the app record",
        &format!(
            "In the Apple Developer portal: the App ID {} with its capabilities, and an App Store provisioning profile for it and your Apple Distribution certificate. In App Store Connect: the app record (the API cannot create one); put its numeric Apple ID in icm.toml [ios] asc_app_id.",
            config.app.id
        ),
    ));
    plan.push(OwnerStep::manual(
        StepKind::Once,
        "Install the API key",
        &format!(
            "Save the key as ~/.appstoreconnect/private_keys/AuthKey_<key id>.p8, where altool finds it, and export {key} and {issuer} in your shell."
        ),
    ));
    plan.push(OwnerStep::manual(
        StepKind::Web,
        "Fill in the listing in App Store Connect",
        &listing_note(
            c,
            "Screenshots per device class (6.9-inch iPhone 1320x2868, or 6.5-inch: `icm run ios-sim --store`, then `icm shot ios-sim --store` for each screen), the description, the App Privacy answers and the age rating.",
        ),
    ));
    if config.ios.uses_non_exempt_encryption == Some(true)
        && config.ios.export_compliance_code.is_none()
    {
        plan.push(OwnerStep::manual(
            StepKind::Web,
            "Upload the export compliance documentation",
            "The app declares non-exempt encryption without [ios] export_compliance_code: App Store Connect asks for the documentation with every build until it has a code.",
        ));
    }

    let api = |words: &mut Vec<Word>| {
        words.extend([
            lit("--api-key"),
            Word::env(key),
            lit("--api-issuer"),
            Word::env(issuer),
        ]);
    };
    let mut validate = vec![
        lit("xcrun"),
        lit("altool"),
        lit("--validate-app"),
        Word::dist(ipa),
    ];
    api(&mut validate);
    validate.extend([lit("--output-format"), lit("json")]);
    plan.push(OwnerStep::run(
        StepKind::Upload,
        "Validate the build with App Store Connect",
        cmd(validate).tee("validate.json").diagnose("altool"),
    ));

    let mut upload = vec![
        lit("xcrun"),
        lit("altool"),
        lit("--upload-package"),
        Word::dist(ipa),
    ];
    api(&mut upload);
    upload.extend([lit("--wait"), lit("--output-format"), lit("json")]);
    plan.push(
        OwnerStep::run(
            StepKind::Upload,
            "Upload the build",
            cmd(upload).tee("upload.json").diagnose("altool"),
        )
        .note("altool's exit code is not trusted: `icm diagnose altool` reads its JSON and prints the delivery id."),
    );
    plan.push(OwnerStep::manual(
        StepKind::Upload,
        &format!("Or, instead of altool: Transporter (the Mac App Store app) uploads {ipa} by drag and drop, signed in with your Apple ID"),
        "The build-status step below then confirms it the same way.",
    ));

    let apple_id = match &config.ios.asc_app_id {
        Some(id) => lit(id),
        None => {
            plan.need(
                "ASC_APP_ID",
                "the app's numeric App Store Connect id (or set [ios] asc_app_id in icm.toml)",
            );
            Word::env("ASC_APP_ID")
        }
    };
    let mut status = vec![
        lit("xcrun"),
        lit("altool"),
        lit("--build-status"),
        lit("--apple-id"),
        apple_id,
        lit("--bundle-version"),
        lit(&c.build.to_string()),
        lit("--bundle-short-version-string"),
        lit(c.version),
        lit("--platform"),
        lit("ios"),
    ];
    api(&mut status);
    status.extend([lit("--wait"), lit("--output-format"), lit("json")]);
    plan.push(
        OwnerStep::run(
            StepKind::Upload,
            "Confirm the build reached this app",
            cmd(status).tee("build-status.json").diagnose("altool"),
        )
        .note("Checks the build landed in the app with this Apple ID (altool can pick the wrong app when bundle ids share a prefix), and waits while App Store Connect processes it; then it shows in TestFlight. `--delivery-id <id>` from the upload's JSON works too."),
    );
    plan.push(mark_uploaded(c));
    plan
}

/// The jarsigner line for an AAB icm could not sign (design §11.2 step
/// 6), run by the owner instead of `icm release android` with the
/// variables set.
pub fn android_sign(c: &Common<'_>, unsigned: &str, signed: &str) -> Option<OwnerStep> {
    let upload = c.config.android.signing.as_ref()?.upload.as_ref()?;
    let key_pass = upload
        .key_pass_env
        .as_deref()
        .unwrap_or(&upload.store_pass_env);
    Some(
        OwnerStep::run(
            StepKind::Once,
            "Sign the bundle with the upload key",
            cmd(vec![
                lit("jarsigner"),
                lit("-J-Duser.language=en"),
                lit("-keystore"),
                lit(&upload.keystore),
                lit("-storepass:env"),
                lit(&upload.store_pass_env),
                lit("-keypass:env"),
                lit(key_pass),
                lit("-sigalg"),
                lit("SHA256withRSA"),
                lit("-digestalg"),
                lit("SHA-256"),
                lit("-signedjar"),
                Word::dist(signed),
                Word::dist(unsigned),
                lit(&upload.alias),
            ]),
        )
        .note(&format!(
            "Simpler: export {} and rerun `icm release android`, which signs and verifies the bundle. The line assumes an RSA key (SHA256withECDSA for an EC key).",
            upload.store_pass_env
        )),
    )
}

/// Google Play (design §11.2): the first release goes through the Play
/// Console by hand; later ones through fastlane supply.
pub fn android(c: &Common<'_>, aab: &str, symbols: Option<&str>, icon: Option<&str>) -> OwnerPlan {
    let config = c.config;
    let mut plan = OwnerPlan::new("Google Play");
    if config.android.signing.is_none() {
        plan.push(
            OwnerStep::run(
                StepKind::Once,
                "Create the upload key",
                cmd(vec![
                    lit("keytool"),
                    lit("-genkeypair"),
                    lit("-v"),
                    lit("-keystore"),
                    Word(vec![
                        Part::Env("HOME".into()),
                        Part::Lit("/.icm/keys/app-upload.jks".into()),
                    ]),
                    lit("-alias"),
                    lit("upload"),
                    lit("-keyalg"),
                    lit("RSA"),
                    lit("-keysize"),
                    lit("4096"),
                    lit("-validity"),
                    lit("9125"),
                    lit("-storetype"),
                    lit("PKCS12"),
                ]),
            )
            .note("keytool prompts for the passwords: keep them in your password manager. Then set [android.signing] upload in icm.toml (the passwords by variable name only) and export the variables."),
        );
    }
    plan.push(OwnerStep::manual(
        StepKind::Web,
        "Fill in the listing in the Play Console",
        &listing_note(
            c,
            &format!(
                "The 512 px icon{}, a 1024x500 feature graphic, at least 2 phone screenshots, the content rating, the Data safety form (it must match the manifest's permissions) and the target audience. A personal developer account created after 2023-11-13 needs a closed test with at least 12 testers for 14 days before production.",
                icon.map(|icon| format!(" ({icon} here)")).unwrap_or_default()
            ),
        ),
    ));
    if let Some(symbols) = symbols {
        plan.push(OwnerStep::manual(
            StepKind::Web,
            "Upload the native debug symbols",
            &format!("In the Play Console's App bundle explorer, upload {symbols} for this build, so crash reports are symbolicated."),
        ));
    }

    if c.first_upload {
        plan.manual_only = Some(format!(
            "This is the app's first Android release: Google Play takes it only through the Play Console. Create the app, upload {aab} to Internal testing by hand, then run `icm ledger mark-uploaded android --build {}`. Later releases upload with upload.sh.",
            c.build
        ));
        plan.push(OwnerStep::manual(
            StepKind::Web,
            "Create the app and upload this bundle by hand",
            &format!("Play Console > Create app ({}), then Testing > Internal testing > Create new release with {aab}.", config.app.id),
        ));
        plan.push(mark_uploaded(c));
        return plan;
    }

    let account = config.android.play.service_account_json_env.as_str();
    plan.need(
        account,
        "the path of the Play Console service account's JSON key",
    );
    plan.push(
        OwnerStep::run(
            StepKind::Upload,
            "Upload the bundle as a draft",
            cmd(vec![
                lit("fastlane"),
                lit("supply"),
                lit("--aab"),
                Word::dist(aab),
                lit("--package_name"),
                lit(&config.app.id),
                lit("--track"),
                lit(&config.android.play.track),
                lit("--release_status"),
                lit("draft"),
                lit("--json_key"),
                Word::env(account),
            ])
            .tee("supply.log")
            .diagnose("play"),
        )
        .note("Needs fastlane 2.240 or newer. Then review and roll out the draft in the Play Console."),
    );
    plan.push(mark_uploaded(c));
    plan
}

/// A static web host (design §11.3), chosen by `[web] host`.
pub fn web(c: &Common<'_>, site: &str) -> OwnerPlan {
    let web = &c.config.web;
    let mut plan = OwnerPlan::new(match web.host {
        WebHost::CloudflarePages => "Cloudflare Pages",
        WebHost::Netlify => "Netlify",
        WebHost::GithubPages => "GitHub Pages",
        WebHost::S3 => "Amazon S3",
        WebHost::Generic => "a static web host",
    });
    let project = |plan: &mut OwnerPlan| -> Word {
        if web.project.trim().is_empty() {
            plan.need(
                "WEB_PROJECT",
                "the host's project or bucket name (or set [web] project in icm.toml)",
            );
            Word::env("WEB_PROJECT")
        } else {
            lit(&web.project)
        }
    };
    let site_dir = format!("{}/", site.trim_end_matches('/'));
    match web.host {
        WebHost::CloudflarePages => {
            let name = project(&mut plan);
            plan.push(
                OwnerStep::run(
                    StepKind::Upload,
                    "Deploy the site",
                    cmd(vec![
                        lit("npx"),
                        lit("wrangler"),
                        lit("pages"),
                        lit("deploy"),
                        Word::dist(site),
                        lit("--project-name"),
                        name,
                    ]),
                )
                .note("wrangler uses `wrangler login`, or CLOUDFLARE_API_TOKEN and CLOUDFLARE_ACCOUNT_ID."),
            );
        }
        WebHost::Netlify => {
            let mut words = vec![
                lit("npx"),
                lit("netlify"),
                lit("deploy"),
                lit("--dir"),
                Word::dist(site),
                lit("--prod"),
            ];
            if !web.project.trim().is_empty() {
                words.extend([lit("--site"), lit(&web.project)]);
            }
            plan.push(
                OwnerStep::run(StepKind::Upload, "Deploy the site", cmd(words))
                    .note("netlify uses `netlify login`, or NETLIFY_AUTH_TOKEN."),
            );
        }
        WebHost::S3 => {
            let bucket = project(&mut plan);
            let s3 = |bucket: &Word| {
                let mut parts = vec![Part::Lit("s3://".into())];
                parts.extend(bucket.0.iter().cloned());
                parts.push(Part::Lit("/".into()));
                Word(parts)
            };
            plan.push(OwnerStep::run(
                StepKind::Upload,
                "Upload the site",
                cmd(vec![
                    lit("aws"),
                    lit("s3"),
                    lit("sync"),
                    Word::dist(site),
                    s3(&bucket),
                    lit("--delete"),
                ]),
            ));
            plan.push(
                OwnerStep::run(
                    StepKind::Upload,
                    "Serve the .wasm as application/wasm",
                    cmd(vec![
                        lit("aws"),
                        lit("s3"),
                        lit("cp"),
                        Word::dist(site),
                        s3(&bucket),
                        lit("--recursive"),
                        lit("--exclude"),
                        lit("*"),
                        lit("--include"),
                        lit("*.wasm"),
                        lit("--content-type"),
                        lit("application/wasm"),
                        lit("--metadata-directive"),
                        lit("REPLACE"),
                    ]),
                )
                .note("Browsers compile a .wasm only when it is served as application/wasm."),
            );
        }
        WebHost::GithubPages => {
            plan.manual_only = Some(format!(
                "GitHub Pages publishes from a workflow, not from this machine: upload {site_dir} with actions/upload-pages-artifact and deploy it with actions/deploy-pages."
            ));
            plan.push(OwnerStep::manual(
                StepKind::Web,
                "Publish with a GitHub Pages workflow",
                &format!("Enable Pages (Settings > Pages > Source: GitHub Actions) and publish {site_dir} from the workflow."),
            ));
        }
        WebHost::Generic => {
            plan.need(
                "WEB_DEPLOY_TARGET",
                "where rsync copies the site, e.g. user@host:/var/www/app",
            );
            plan.push(OwnerStep::run(
                StepKind::Upload,
                "Copy the site to the host",
                cmd(vec![
                    lit("rsync"),
                    lit("-av"),
                    lit("--delete"),
                    Word::dist(&site_dir),
                    Word(vec![
                        Part::Env("WEB_DEPLOY_TARGET".into()),
                        Part::Lit("/".into()),
                    ]),
                ]),
            ));
        }
    }

    let url = if web.public_url.starts_with("https://") {
        lit(&web.public_url)
    } else {
        plan.need("WEB_URL", "the deployed site's https:// URL");
        Word::env("WEB_URL")
    };
    let mut verify = vec![lit("icm"), lit("verify"), lit("web"), lit("--url"), url];
    verify.extend(c.config_arg());
    plan.push(
        OwnerStep::run(StepKind::After, "Check the deployed site", cmd(verify))
            .note("Loads the real host in headless Chrome: the .wasm's MIME type, ICM_EVENT ready, a drawn page."),
    );
    plan.push(mark_uploaded(c));
    plan
}

fn notary_profile(c: &Common<'_>) -> String {
    c.config
        .desktop
        .macos
        .notary_profile
        .clone()
        .unwrap_or_else(|| "icm-notary".to_string())
}

fn store_credentials(c: &Common<'_>, profile: &str) -> OwnerStep {
    let key = c.config.store.asc_key_id_env.as_str();
    let issuer = c.config.store.asc_issuer_id_env.as_str();
    OwnerStep::run(
        StepKind::Once,
        "Store the notary credentials in the keychain",
        cmd(vec![
            lit("xcrun"),
            lit("notarytool"),
            lit("store-credentials"),
            lit(profile),
            lit("--key"),
            Word(vec![
                Part::Env("HOME".into()),
                Part::Lit("/.appstoreconnect/private_keys/AuthKey_".into()),
                Part::Env(key.into()),
                Part::Lit(".p8".into()),
            ]),
            lit("--key-id"),
            Word::env(key),
            lit("--issuer"),
            Word::env(issuer),
        ]),
    )
    .note(&format!(
        "Once per Mac, with {key} and {issuer} exported; later commands name only the keychain profile {profile}, so no secret appears in them. Set [desktop.macos] notary_profile to use another name."
    ))
}

fn notarize(file: &str, profile: &str, saved: &str) -> Command {
    cmd(vec![
        lit("xcrun"),
        lit("notarytool"),
        lit("submit"),
        Word::dist(file),
        lit("--keychain-profile"),
        lit(profile),
        lit("--wait"),
        lit("--timeout"),
        lit("30m"),
        lit("--output-format"),
        lit("json"),
    ])
    .tee(saved)
    .diagnose("notarytool")
}

/// macOS stage 1 (design §11.4): notarize and staple the app, then build
/// the DMG (`icm release macos --dmg`).
pub fn macos_app(c: &Common<'_>, app_zip: &str, app: &str) -> OwnerPlan {
    let profile = notary_profile(c);
    let mut plan = OwnerPlan::new("Apple's notary service (stage 1 of 2: the app)");
    plan.push(store_credentials(c, &profile));
    plan.push(OwnerStep::run(
        StepKind::Upload,
        "Notarize the app",
        notarize(app_zip, &profile, "notary-app.json"),
    ));
    plan.push(OwnerStep::run(
        StepKind::Upload,
        "Staple the ticket to the app",
        cmd(vec![
            lit("xcrun"),
            lit("stapler"),
            lit("staple"),
            Word::dist(app),
        ]),
    ));
    let mut dmg = vec![lit("icm"), lit("release"), lit("macos"), lit("--dmg")];
    dmg.extend(c.config_arg());
    plan.push(
        OwnerStep::run(
            StepKind::After,
            "Build the DMG of the stapled app (stage 2)",
            cmd(dmg),
        )
        .note("Its UPLOAD.md has stage 2: notarize and staple the DMG."),
    );
    plan
}

/// macOS stage 2 (design §11.4): notarize and staple the DMG, check
/// Gatekeeper, record it.
pub fn macos_dmg(c: &Common<'_>, dmg: &str) -> OwnerPlan {
    let profile = notary_profile(c);
    let mut plan = OwnerPlan::new("Apple's notary service (stage 2 of 2: the DMG)");
    plan.push(store_credentials(c, &profile));
    plan.push(OwnerStep::run(
        StepKind::Upload,
        "Notarize the DMG",
        notarize(dmg, &profile, "notary-dmg.json"),
    ));
    plan.push(OwnerStep::run(
        StepKind::Upload,
        "Staple the ticket to the DMG",
        cmd(vec![
            lit("xcrun"),
            lit("stapler"),
            lit("staple"),
            Word::dist(dmg),
        ]),
    ));
    let mut verify = vec![
        lit("icm"),
        lit("verify"),
        lit("macos"),
        lit("--after-notarize"),
    ];
    verify.extend(c.config_arg());
    plan.push(
        OwnerStep::run(
            StepKind::After,
            "Check Gatekeeper accepts both",
            cmd(verify),
        )
        .note("spctl on the app and the DMG, and stapler validate on both."),
    );
    plan.push(OwnerStep::manual(
        StepKind::After,
        "Publish the DMG",
        &format!("Attach {dmg} to a GitHub release or put it on your site."),
    ));
    plan.push(mark_uploaded(c));
    plan
}

/// Windows and Linux (design §11.5, §11.6): no store; the files go to a
/// GitHub release or the owner's site.
pub fn desktop(c: &Common<'_>, files: &[&str]) -> OwnerPlan {
    let mut plan = OwnerPlan::new("a GitHub release (or your own site)");
    plan.need(
        "RELEASE_TAG",
        "the GitHub release to attach the files to, e.g. v1.0.0",
    );
    let mut words = vec![
        lit("gh"),
        lit("release"),
        lit("upload"),
        Word::env("RELEASE_TAG"),
    ];
    words.extend(files.iter().map(|file| Word::dist(*file)));
    plan.push(
        OwnerStep::run(
            StepKind::Upload,
            "Attach the installers to the release",
            cmd(words),
        )
        .note("Needs the GitHub CLI, logged in (`gh auth login`). Any web host works instead."),
    );
    plan.push(mark_uploaded(c));
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn config(extra: &str) -> IcmToml {
        crate::config::parse(
            Path::new("/p/icm.toml"),
            &format!(
                "schema = 1\n[app]\nname = \"Notes\"\nid = \"com.acme.notes\"\nbuild = 12\n{extra}"
            ),
        )
        .unwrap_or_else(|errors| panic!("{errors:?}"))
        .config
    }

    fn common<'a>(target: ReleaseTarget, config: &'a IcmToml, first: bool) -> Common<'a> {
        Common {
            target,
            config,
            version: "1.0.0",
            build: 12,
            first_upload: first,
            icm_toml: Path::new("/p/icm.toml"),
        }
    }

    fn lines(plan: &OwnerPlan) -> Vec<String> {
        plan.steps
            .iter()
            .filter_map(|step| step.command.as_ref().map(Command::shell))
            .collect()
    }

    #[test]
    fn ios_validates_uploads_and_confirms() {
        let config = config("[ios]\nasc_app_id = \"1234567890\"\n");
        let plan = ios(&common(ReleaseTarget::Ios, &config, false), "Notes.ipa");
        assert!(plan.undeclared_vars().is_empty());
        let lines = lines(&plan);
        assert_eq!(
            lines[0],
            "xcrun altool --validate-app \"$D/Notes.ipa\" --api-key \"${ASC_KEY_ID}\" --api-issuer \"${ASC_ISSUER_ID}\" --output-format json | tee \"$D/validate.json\""
        );
        assert!(lines[1].contains("--upload-package \"$D/Notes.ipa\""));
        assert!(lines[2].contains(
            "--build-status --apple-id 1234567890 --bundle-version 12 --bundle-short-version-string 1.0.0 --platform ios"
        ));
        assert_eq!(
            lines[3],
            "icm ledger mark-uploaded ios --build 12 --config /p/icm.toml"
        );
        // Without asc_app_id, the id comes from a variable upload.sh checks.
        let bare = config_without_id();
        let plan = ios(&common(ReleaseTarget::Ios, &bare, false), "Notes.ipa");
        assert!(plan.env.iter().any(|need| need.name == "ASC_APP_ID"));
        assert!(
            lines_of(&plan)
                .iter()
                .any(|l| l.contains("--apple-id \"${ASC_APP_ID}\""))
        );
    }

    fn config_without_id() -> IcmToml {
        config("")
    }

    fn lines_of(plan: &OwnerPlan) -> Vec<String> {
        lines(plan)
    }

    #[test]
    fn android_first_release_is_manual_then_fastlane() {
        let config = config(
            "[android.signing]\nupload = { keystore = \"/k/up.jks\", alias = \"upload\", store_pass_env = \"STORE_PASS\" }\n",
        );
        let first = android(
            &common(ReleaseTarget::Android, &config, true),
            "notes-1.0.0-12.aab",
            Some("native-debug-symbols.zip"),
            Some("play-icon-512.png"),
        );
        assert!(
            first
                .manual_only
                .as_deref()
                .unwrap()
                .contains("first Android release")
        );
        assert!(!lines(&first).iter().any(|l| l.contains("fastlane")));

        let later = android(
            &common(ReleaseTarget::Android, &config, false),
            "notes-1.0.0-12.aab",
            None,
            None,
        );
        assert!(later.manual_only.is_none());
        assert!(later.undeclared_vars().is_empty());
        let lines = lines(&later);
        assert_eq!(
            lines[0],
            "fastlane supply --aab \"$D/notes-1.0.0-12.aab\" --package_name com.acme.notes --track internal --release_status draft --json_key \"${PLAY_SERVICE_ACCOUNT_JSON}\" | tee \"$D/supply.log\""
        );

        let sign = android_sign(
            &common(ReleaseTarget::Android, &config, false),
            "unsigned.aab",
            "notes.aab",
        )
        .unwrap();
        let line = sign.command.unwrap().shell();
        assert!(
            line.contains("-storepass:env STORE_PASS -keypass:env STORE_PASS"),
            "{line}"
        );
        assert!(!line.contains("-storepass "), "{line}");
    }

    #[test]
    fn web_hosts_get_their_commands() {
        for (host, expect) in [
            (
                "cloudflare-pages",
                "npx wrangler pages deploy \"$D/site\" --project-name notes",
            ),
            (
                "netlify",
                "npx netlify deploy --dir \"$D/site\" --prod --site notes",
            ),
            ("s3", "aws s3 sync \"$D/site\" s3://notes/ --delete"),
            (
                "generic",
                "rsync -av --delete \"$D/site/\" \"${WEB_DEPLOY_TARGET}/\"",
            ),
        ] {
            let config = config(&format!(
                "[web]\nhost = \"{host}\"\nproject = \"notes\"\npublic_url = \"https://notes.dev/\"\n"
            ));
            let plan = web(&common(ReleaseTarget::Web, &config, false), "site");
            assert!(plan.undeclared_vars().is_empty(), "{host}");
            let lines = lines(&plan);
            assert_eq!(lines[0], expect, "{host}");
            assert!(
                lines
                    .iter()
                    .any(|l| l.contains("icm verify web --url https://notes.dev/"))
            );
        }
        let pages = config("[web]\nhost = \"github-pages\"\n");
        let plan = web(&common(ReleaseTarget::Web, &pages, false), "site");
        assert!(plan.manual_only.is_some());
        assert!(plan.env.iter().any(|need| need.name == "WEB_URL"));
    }

    #[test]
    fn macos_notarizes_with_a_keychain_profile() {
        let config = config("[desktop.macos]\nnotary_profile = \"acme-notary\"\n");
        let c = common(ReleaseTarget::Macos, &config, false);
        let stage1 = macos_app(&c, "Notes-1.0.0.app.zip", "Notes.app");
        assert!(stage1.undeclared_vars().is_empty());
        let lines = lines(&stage1);
        assert!(lines[0].starts_with("xcrun notarytool store-credentials acme-notary --key \"${HOME}/.appstoreconnect/private_keys/AuthKey_${ASC_KEY_ID}.p8\""));
        assert!(
            lines[1].contains(
                "submit \"$D/Notes-1.0.0.app.zip\" --keychain-profile acme-notary --wait"
            )
        );
        assert_eq!(lines[2], "xcrun stapler staple \"$D/Notes.app\"");
        assert!(lines[3].starts_with("icm release macos --dmg"));
        let stage2 = macos_dmg(&c, "Notes-1.0.0.dmg");
        assert!(
            lines_of(&stage2)
                .iter()
                .any(|l| l.starts_with("icm verify macos --after-notarize"))
        );
    }

    #[test]
    fn desktop_files_go_to_a_release() {
        let config = config("");
        let plan = desktop(
            &common(ReleaseTarget::Windows, &config, false),
            &["Notes-1.0.0.msi", "Notes-1.0.0-setup.exe"],
        );
        assert_eq!(
            lines(&plan)[0],
            "gh release upload \"${RELEASE_TAG}\" \"$D/Notes-1.0.0.msi\" \"$D/Notes-1.0.0-setup.exe\""
        );
    }

    /// Design §17 item 7: no file but this one contains upload, publish or
    /// notarize argv.
    #[test]
    fn upload_argv_lives_only_here() {
        let forbidden = [
            "--upload-package",
            "--upload-app",
            "notarytool submit",
            "\"notarytool\", \"submit\"",
            "lit(\"submit\")",
            "fastlane supply",
            "lit(\"supply\")",
            "wrangler",
            "netlify deploy",
            "lit(\"deploy\")",
            "s3 sync",
            "lit(\"sync\")",
            "release upload",
            "lit(\"upload\")",
            "edits.insert",
            "edits.commit",
            "uploadType",
            "stapler staple",
            "lit(\"staple\")",
        ];
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut stack = vec![src.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|ext| ext != "rs")
                    || path.ends_with("release/owner_plans.rs")
                {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for pattern in forbidden {
                    if text.contains(pattern) {
                        offenders.push(format!("{}: {pattern}", path.display()));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "upload argv outside owner_plans.rs: {offenders:#?}"
        );
    }
}
