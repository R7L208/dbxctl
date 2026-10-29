# Workspace and Auth Context

> **Partly planned.** The precedence below was observed on Databricks CLI
> 1.13.0 unless a row says otherwise. Recording and checking the context in
> `probe run` is planned in #55 and not implemented yet.

`dbxctl probe` runs every CLI call that contacts the workspace (the bundle
commands, `api get`, and SQL statements) through the shared `CliRunner`, from
the bundle root with `-t <target>` and `-p <profile>`, and passes its
environment through unchanged, so the CLI alone decides which workspace and
credentials a probe uses. The offline calls (`version`, `--help`, and
`bundle schema`) take neither flag. This page records how the CLI makes that decision when a bundle target, a profile,
and environment variables are combined, and what `probe run` will do about it.

## How the Results Were Observed

Each case ran `databricks auth describe` (text and `-o json`) with an empty
environment, a throwaway `DATABRICKS_CONFIG_FILE`, fake tokens, and `.invalid`
hosts, so no workspace was contacted. `--sensitive` was never passed. Every run
ended in a DNS failure or a configuration error, which is expected; the cases
differ in which host, credentials, and sources the CLI reports.

## Profiles and Environment Variables

| Setup | Result |
| --- | --- |
| `-p A` | Host and token come from profile A. |
| `-p A` with `DATABRICKS_HOST` or `DATABRICKS_TOKEN` set | Both are ignored when the profile sets them. Host and token still come from A, and the variables appear nowhere in the output. |
| `-p` naming a profile without a token, with `DATABRICKS_TOKEN` set | The token comes from the variable and is reported as such. |
| `-p` naming a profile without a host, with `DATABRICKS_HOST` set | The host comes from the variable and is reported as such. |
| `DATABRICKS_CONFIG_PROFILE=B` | Profile B is used. |
| `DATABRICKS_CONFIG_PROFILE=B` with `-p A` | `-p` wins silently; profile A is used. |
| `-p A` with `DATABRICKS_AUTH_TYPE` set to another type (1.21.0) | Ignored silently; the auth type stays the profile's. |
| `-p A` (a token profile) with `DATABRICKS_CLIENT_ID` and `DATABRICKS_CLIENT_SECRET` set | Fails: `more than one authorization method configured: oauth and pat`. The `ARM_*` Azure variables fail the same way (`azure and pat`). |
| `DATABRICKS_HOST` and `DATABRICKS_TOKEN` only | Both come from the environment. |
| Nothing set, no `[DEFAULT]` profile | Fails: `cannot configure default credentials`. |
| Nothing set, with a `[DEFAULT]` profile (1.21.0) | **DEFAULT is used silently.** The output shows `profile: DEFAULT` with no source, and the run proceeds. `databricks configure` creates this profile. |
| Only `DATABRICKS_HOST` set, with a `[DEFAULT]` profile (1.21.0) | DEFAULT is not consulted; the run fails for lack of credentials. |

## Bundle Targets

The bundle in these cases sets `workspace.host` on each target and no
`workspace.profile`. Every workspace call a probe makes passes `-t`, so **this is the mode probes
run in**; the table above applies to probes only through the profile a target
resolves to.

| Setup | Result |
| --- | --- |
| `-t dev`, and two profiles match the target's host | Fails: `Matching workspace profiles: A, A2`, with suggestions to set a profile. |
| `-t` where one profile matches the target's host | Proceeds with the target's host. The output does not say which profile supplied the credentials. |
| `-t dev -p A`, matching host | Host comes from the bundle; profile from the flag. |
| `-t dev -p B`, different host | Fails: `the host in the profile (…) doesn’t match the host configured in the bundle (…)`. |
| `-t dev -p A` with `DATABRICKS_HOST` set to another host | The variable is ignored; the host comes from the bundle. |
| `-t dev` with `DATABRICKS_CONFIG_PROFILE` naming a profile with a different host | Fails with the same host-mismatch error. |

In bundle mode `auth describe` cannot attribute the credentials:

- The configuration listing omits the token, so whether a `DATABRICKS_TOKEN`
  was used, or set and ignored, cannot be read from it.
- On 1.21.0 it reports `profile: default` even when the profile came from
  `DATABRICKS_CONFIG_PROFILE` or was the single profile matching the target's
  host.

## What `auth describe` Reports

- `-o json` gives `status`, `error`, and `details` with `auth_type`, `host`,
  and a `configuration` map. Each entry has a `value`, an `auth_type_mismatch`
  flag, and a `source` object such as
  `{"type": "environment variable", "name": "DATABRICKS_CLIENT_ID"}`. The
  `type` is `flag`, `environment variable`, `config file`, `bundle`, or
  `dynamic configuration`. Secrets are shown as `********` unless `--sensitive`
  is passed.
- The details are reported even when authentication fails, including when the
  host cannot be reached.
- **The exit code is 0 in every case above**, success or failure, in both text
  and JSON output.
- **In JSON, a configuration error is an empty object** (`"error": {}`); only
  network errors carry detail. The message, such as the conflicting methods or
  the host mismatch, appears only in text output.

## Compared With the Documentation

The [unified authentication](https://docs.databricks.com/aws/en/dev-tools/auth/unified-auth)
page lists the order in which Databricks tools look for settings: direct
configuration first, then environment variables, then a
[configuration profile](https://docs.databricks.com/aws/en/dev-tools/auth/config-profiles).
That order suggests `DATABRICKS_HOST` would override the host of a profile
named with `-p`. On CLI 1.13.0 it does not: once a profile is named, its own
values win and the environment only fills fields the profile leaves empty. With
a bundle target, the target's host wins over `DATABRICKS_HOST`.

## What This Means for Probes

- A `DATABRICKS_HOST` that disagrees with the target or the profile does not
  move probes to another workspace: it is ignored, or the run fails.
- A host, credential, auth-type, or profile variable that is set but ignored is
  silent. That is the ambiguous case `probe run` should refuse.
- Without `-p`, the CLI can pick a profile itself: an implicit `[DEFAULT]`, or
  the single profile matching the target's host. Nothing in the output says it
  did.
- Two credential types, or a profile whose host differs from the target's,
  already fail in the CLI, but with exit 0 and a message only in text output.

## Planned for `probe run` (#55)

Not implemented. At the start of `probe run`, through the shared runner:

1. Run `databricks auth describe` with the run's `-t` and `-p` twice, once with
   `-o json` and once in text, and add the command to
   `safety::READ_ONLY_COMMANDS`. The JSON is the record; the text is read only
   for the error message. Neither exit code means success: judge the result by
   parsing the text for an error, and refuse if the two calls disagree on the
   host or auth type.

1. Record in `run.json` the workspace host (redacted in reports), auth type,
   identity when available (redacted), bundle target, and profile, with the
   `source` the CLI reports for each. Where it reports none, as for the token
   and profile in bundle mode, record the source as unreported rather than
   inferring one. Never record a secret.

1. Stop with exit 1 before any check runs when:

   - `-p` is given and any of these variables is set, since the profile may
     silently override it: `DATABRICKS_HOST`, `DATABRICKS_TOKEN`,
     `DATABRICKS_CLIENT_ID`, `DATABRICKS_CLIENT_SECRET`,
     `DATABRICKS_AUTH_TYPE`, `DATABRICKS_CONFIG_PROFILE`, `ARM_CLIENT_ID`,
     `ARM_CLIENT_SECRET`, `ARM_TENANT_ID`, or `GOOGLE_CREDENTIALS`;
   - the CLI reports more than one authorization method, a profile and target
     host mismatch, or several matching profiles.

   The message names the conflicting sources.

1. Without `-p`, a credential from the environment and one from a host-matched
   profile cannot be told apart in bundle mode. Record which of the variables
   above were set and mark the credential source as unreported; whether to
   refuse this case too is left to #55.

1. Test each case above with the fake CLI, passing environment variables to the
   child process only. These tests pin how `dbxctl` reads the CLI's output, not
   the CLI's precedence: re-run the observations above against the real CLI
   whenever the tested CLI version changes.

This is outside the milestone 0.0.1 contract, since Phase 0 probes are
read-only. It should still land before the opt-in mutation probes (#37), and
ideally before the first real-workspace run (#35).
