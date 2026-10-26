# Workspace and Auth Context

> **Partly planned.** The precedence below was observed on Databricks CLI
> 1.13.0. Recording and checking the context in `probe run` is planned in #55
> and not implemented yet.

`dbxctl probe` forwards `-t <target>` and `-p <profile>` to every CLI call and
passes its environment through unchanged, so the CLI alone decides which
workspace and credentials a probe uses. This page records how CLI 1.13.0 makes
that decision when a bundle target, a profile, and `DATABRICKS_*` environment
variables are combined, and what `probe run` will do about it.

## How the Results Were Observed

Each case ran `databricks auth describe` (text and `-o json`) from CLI 1.13.0
with an empty environment, a throwaway `DATABRICKS_CONFIG_FILE`, fake tokens,
and `.invalid` hosts, so no workspace was contacted. `--sensitive` was never
passed. Every run ended in a DNS failure or a configuration error, which is
expected; the cases differ in which host, credentials, and sources the CLI
reports.

## Profiles and Environment Variables

| Setup | Result |
| --- | --- |
| `-p A` | Host and token come from profile A. |
| `-p A` with `DATABRICKS_HOST` or `DATABRICKS_TOKEN` set | Both are ignored when the profile sets them. Host and token still come from A, and the variables appear nowhere in the output. |
| `-p` naming a profile without a token, with `DATABRICKS_TOKEN` set | The token comes from the variable and is reported as such. |
| `-p` naming a profile without a host, with `DATABRICKS_HOST` set | The host comes from the variable and is reported as such. |
| `DATABRICKS_CONFIG_PROFILE=B` | Profile B is used. |
| `DATABRICKS_CONFIG_PROFILE=B` with `-p A` | `-p` wins; profile A is used. |
| `-p A` (a token profile) with `DATABRICKS_CLIENT_ID` and `DATABRICKS_CLIENT_SECRET` set | Fails: `more than one authorization method configured: oauth and pat`. |
| `DATABRICKS_HOST` and `DATABRICKS_TOKEN` only | Both come from the environment. |
| Nothing set | Fails: `cannot configure default credentials`. |

## Bundle Targets

The bundle in these cases sets `workspace.host` on each target and no
`workspace.profile`.

| Setup | Result |
| --- | --- |
| `-t dev`, and two profiles match the target's host | Fails: `Matching workspace profiles: A, A2`, with suggestions to set a profile. |
| `-t` where one profile matches the target's host | Proceeds with the target's host. The output does not say which profile supplied the credentials. |
| `-t dev -p A`, matching host | Host comes from the bundle; profile from the flag. |
| `-t dev -p B`, different host | Fails: `the host in the profile (…) doesn’t match the host configured in the bundle (…)`. |
| `-t dev -p A` with `DATABRICKS_HOST` set to another host | The variable is ignored; the host comes from the bundle. |
| `-t dev` with `DATABRICKS_CONFIG_PROFILE` naming a profile with a different host | Fails with the same host-mismatch error. |

In bundle mode the configuration listing omits the token, so whether a
`DATABRICKS_TOKEN` was used cannot be read from `auth describe`.

## What `auth describe` Reports

- `-o json` gives `status`, `error`, and `details` with `auth_type`, `host`,
  and a `configuration` map. Each entry has a `value` and a `source`: `flag`,
  `environment variable`, `config file`, `bundle`, or `dynamic configuration`.
  Secrets are shown as `********` unless `--sensitive` is passed.
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
- A set but ignored `DATABRICKS_HOST` or `DATABRICKS_TOKEN` is silent. That is
  the ambiguous case `probe run` should refuse.
- Two credential types, or a profile whose host differs from the target's,
  already fail in the CLI, but with exit 0 and a message only in text output.

## Planned for `probe run` (#55)

Not implemented. Once at the start of `probe run`, through the shared runner:

1. Run `databricks auth describe` with the run's `-t` and `-p`, in both JSON
   and text output, and add the command to `safety::READ_ONLY_COMMANDS`.

1. Record in `run.json` the workspace host (redacted in reports), auth type,
   identity when available (redacted), bundle target, and profile, and the
   source of each. Never record a secret.

1. Stop with exit 1 before any check runs when:

   - a `DATABRICKS_*` variable that sets the host or a credential is present
     but another source won;
   - the CLI reports more than one authorization method, a profile and target
     host mismatch, or several matching profiles.

   The message names the conflicting sources.

1. Test each case above with the fake CLI, passing environment variables to the
   child process only.

Phase 0 probes are read-only, so this is deferred past milestone 0.0.1. It
should land before the opt-in mutation probes (#37), and ideally before the
first real-workspace run (#35).
