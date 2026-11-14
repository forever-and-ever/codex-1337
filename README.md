# Codex 1337

Unofficial fork of [openai/codex](https://github.com/openai/codex); not
affiliated with or endorsed by OpenAI.

Built and tested only on Linux; code for other platforms is inherited from
upstream and not validated. Local builds carry the `-1337` suffix. Install the
coordinated distribution with `scripts/install-local`.

The shared app server, account scoping, and agent controls belong to this fork.
Use `codex agents` for the session dashboard. The graphical Wrangler is retired.
The distribution owns its systemd user units; upgrades wait for idle without
closing admission or forcibly restarting the shared server. The federation bridge
remains bundled.

`/reload` replaces the Linux UI without detaching its server connection or stopping
agents. Drafts, attachments, queued input, steering, inline answers, and the cut
buffer survive; the view is rebuilt. Replacement and fallback share a five-second
deadline, then offer retry/recovery. Open dialogs and unsettled UI actions defer
replacement. Compatible installed updates use the same handoff after five quiet
minutes. Older clients need one ordinary restart to acquire the reload host;
host/protocol upgrades also require an ordinary restart.

Subagent navigation is keyboard-only: Down at the end of the composer enters the
list, Enter opens an agent, and Esc returns to the main agent. The fullscreen
transcript uses upstream selection and scrolling. On Linux, selecting text
publishes PRIMARY without replacing CLIPBOARD; terminal paste shortcuts remain available.

On X11/i3, hidden windows or 30 seconds of desktop inactivity stop repainting,
not agent execution. Visible, unfocused windows redraw for content without
animation; focused windows animate normally. SSH always uses reduced presentation.

<p align="center"><strong>Codex CLI</strong> is a coding agent from OpenAI that runs locally on your computer.
<p align="center">
  <img src="https://github.com/openai/codex/blob/main/.github/codex-cli-splash.png" alt="Codex CLI splash" width="80%" />
</p>
</br>
If you want Codex in your code editor (VS Code, Cursor, Windsurf), <a href="https://developers.openai.com/codex/ide">install in your IDE.</a>
</br>If you want the desktop app experience, run <code>codex app</code> or visit <a href="https://chatgpt.com/codex?app-landing-page=true">the Codex App page</a>.
</br>If you are looking for the <em>cloud-based agent</em> from OpenAI, <strong>Codex Web</strong>, go to <a href="https://chatgpt.com/codex">chatgpt.com/codex</a>.</p>

---

## Quickstart

### Installing and running Codex CLI

Run the following on Mac or Linux to install Codex CLI:

```shell
curl -fsSL https://chatgpt.com/codex/install.sh | sh
```

Run the following on Windows to install Codex CLI:

```shell
powershell -ExecutionPolicy ByPass -c "irm https://chatgpt.com/codex/install.ps1 | iex"
```

The standalone installers download from `https://releases.openai.com/codex` by default and fall back to GitHub Releases if a metadata or asset download is unavailable. To force GitHub Releases, set `CODEX_INSTALLER_USE_RELEASES_OPENAI_COM` to `false` (`0` and `no` are also accepted):

```shell
curl -fsSL https://chatgpt.com/codex/install.sh | CODEX_INSTALLER_USE_RELEASES_OPENAI_COM=false sh
```

```powershell
$env:CODEX_INSTALLER_USE_RELEASES_OPENAI_COM='false'; irm https://chatgpt.com/codex/install.ps1 | iex
```

Codex CLI can also be installed via the following package managers:

```shell
# Install using npm
npm install -g @openai/codex
```

```shell
# Install using Homebrew
brew install --cask codex
```

Then simply run `codex` to get started.

<details>
<summary>You can also go to the <a href="https://github.com/openai/codex/releases/latest">latest GitHub Release</a> and download the appropriate binary for your platform.</summary>

Each GitHub Release contains many executables, but in practice, you likely want one of these:

- macOS
  - Apple Silicon/arm64: `codex-aarch64-apple-darwin.tar.gz`
  - x86_64 (older Mac hardware): `codex-x86_64-apple-darwin.tar.gz`
- Linux
  - x86_64: `codex-x86_64-unknown-linux-musl.tar.gz`
  - arm64: `codex-aarch64-unknown-linux-musl.tar.gz`

Each archive contains a single entry with the platform baked into the name (e.g., `codex-x86_64-unknown-linux-musl`), so you likely want to rename it to `codex` after extracting it.

</details>

### Using Codex with your ChatGPT plan

Run `codex` and select **Sign in with ChatGPT**. We recommend signing into your ChatGPT account to use Codex as part of your Plus, Pro, Business, Edu, or Enterprise plan. [Learn more about what's included in your ChatGPT plan](https://help.openai.com/en/articles/11369540-codex-in-chatgpt).

You can also use Codex with an API key, but this requires [additional setup](https://developers.openai.com/codex/auth#sign-in-with-an-api-key).

### Downstream Accounts

`codex login` asks you to choose ChatGPT (OpenAI) or Claude subscription
(Anthropic) when stdin and stdout are terminals. Use
`codex login --provider openai` or
`codex login --provider claude_subscription` to skip the menu; a bare login
without a terminal requires this explicit choice. OpenAI credential/device-auth
flags imply OpenAI. `codex login status` reports the OpenAI login and all saved
Claude accounts; `--provider` limits the report to one provider. OpenRouter keys
are selected from key files in `/account`, not through CLI sign-in.

Sign-in adds or updates a saved account in the existing credential store;
only a provider's first account becomes its active account. `/account` opens the
picker, including during work; `/account work` uses an alias.
The picker shows account IDs for configuring aliases:

```toml
[wrangler.account_aliases]
work = "user-id:account-id"
```

The conversation and its agents switch independently at model/tool boundaries,
without interrupting in-flight work. Selection also makes the account its
provider's active account: every new conversation starts with the account last
chosen in `/account`, while existing conversations keep theirs. Bindings survive resume;
independent forks inherit once, while children follow the family. A missing
account requires login or another explicit selection,
never an automatic fallback. Per-conversation selection supports personal
accounts only; managed workspace policies remain server-scoped. Credential
paths are unchanged.

Before adding another account, let the old app-server drain and restart. Old
Codex binaries do not preserve the multi-account credential document.

### Direct delegation

Use `/farm <task>` in the main conversation to start a child immediately with the
ordinary full-history fork. `/farm_fresh <task>` starts with empty conversation
history. Both remain in the main view, preserve normal instructions and permissions,
and report through the existing agent family; completion alone does not wake an idle parent.

Configure the dedicated executor in `config.toml`, for example:

```toml
[wrangler.farm]
model = "gpt-6.1-sol"
reasoning_effort = "high"
# instructions = "Additional child-only instructions (at most 512 bytes)."
```

Unspecified model/effort use the general subagent defaults, then the parent's
selection. Farm's executor takes precedence over role model preferences, not role
restrictions. `/farm` cannot switch providers; choose `/farm_fresh` explicitly for
that. Full forks retain the ordinary native-checkpoint compatibility restrictions.
Tasks must fit the existing bounded retained-user authorization limit.

If a receipt is missing, enter `/farm` without a task to check the **same** request
or discard its local recovery state. Discard never cancels a possibly running child;
inspect `/agents` before intentionally submitting another task. Submitted rich drafts
are retained without overwriting newer edits; restore explicitly into an empty composer.
Resolve or discard retained farm requests/drafts before reloading or idle upgrading
the TUI. Abrupt process exit cannot preserve this transient recovery state.

## Docs

- [**Codex Documentation**](https://developers.openai.com/codex)
- [**Contributing**](./docs/contributing.md)
- [**Installing & building**](./docs/install.md)
- [**Open source fund**](./docs/open-source-fund.md)

This repository is licensed under the [Apache-2.0 License](LICENSE).
