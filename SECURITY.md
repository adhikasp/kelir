# Security policy

## Supported versions

Security fixes are applied to the latest published release. There is no
long-term support branch; upgrade to the newest version.

## Reporting a vulnerability

Please report suspected vulnerabilities privately rather than opening a public
issue. Use GitHub's private vulnerability reporting on this repository, or
contact the maintainer through the channel listed on their GitHub profile.

Include, if you can:

- the version and platform (`kelir --version`, OS);
- the affected component (credential handling, session IPC, gate evaluation,
  process spawning, ...);
- a minimal reproduction and the impact you believe it has.

Do **not** include real API keys, tokens or session tokens in a report. If you
have accidentally committed a secret, treat it as compromised and rotate it
first; scrubbing it from history does not undo the exposure.

You should get an acknowledgement within a few days. Once a fix is available,
it will be released and the report credited as the reporter chooses.

## Out of scope

- Vulnerabilities in a model provider's service or in the model's own output.
- Issues that require the attacker to already have the user's full privileges
  and run arbitrary code as that user.
- Social-engineering or phishing against users of the harness.

## Security model in brief

kelir is a fully autonomous local agent: it runs commands with the user's
privileges and does not prompt for approval. Its security boundaries are the
OS user account, owner-only state directories and session files, a scrubbed
environment for spawned children, and optional event gates for policy. See
`docs/SPEC.md` and `docs/reference/{gates,files,children}.md` for details.
