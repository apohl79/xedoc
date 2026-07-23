#!/bin/bash
# Codex status line script
# Reads JSON payload from stdin, outputs ANSI-styled status line
#
# Feature flags — set via environment to override defaults (1=on, 0=off)

# Project directory basename (blue)
ENABLE_DIR=${ENABLE_DIR:-1}
# Git branch + dirty indicator (green)
ENABLE_GIT=${ENABLE_GIT:-1}
# Model name (magenta)
ENABLE_MODEL=${ENABLE_MODEL:-1}
# Context window usage percentage (cyan)
ENABLE_CONTEXT=${ENABLE_CONTEXT:-1}
# Actual token usage / context window size next to the percentage (cyan)
ENABLE_CONTEXT_TOKENS=${ENABLE_CONTEXT_TOKENS:-1}
# Session cost in USD (yellow)
ENABLE_COST=${ENABLE_COST:-1}
# Wall-clock session duration (white)
ENABLE_DURATION=${ENABLE_DURATION:-0}
# Lines added/removed in session (green/red)
ENABLE_LINES=${ENABLE_LINES:-0}
# Cumulative input/output token counts (dim)
ENABLE_TOKENS=${ENABLE_TOKENS:-0}
# Codex version (dim gray)
ENABLE_VERSION=${ENABLE_VERSION:-0}
# Vim mode indicator — NORMAL/INSERT (bold yellow)
ENABLE_VIM_MODE=${ENABLE_VIM_MODE:-0}
# Codex session name (lavender)
ENABLE_SESSION_NAME=${ENABLE_SESSION_NAME:-0}
# Codex task progress indicator (orange)
ENABLE_TASK_INDICATOR=${ENABLE_TASK_INDICATOR:-0}

input=$(</dev/stdin)

status_fields=$(printf '%s' "$input" | jq -r '
    (.context_window.context_window_size // 0) as $ctx_size |
    (.context_window.used_percentage // 0) as $ctx_pct |
    (.context_window.current_usage // {}) as $cu |
    (($cu.input_tokens // 0) + ($cu.cache_creation_input_tokens // 0)
     + ($cu.cache_read_input_tokens // 0) + ($cu.output_tokens // 0)) as $ctx_used |
    (if (.task_indicator | type) == "object" then
        (.task_indicator.text //
         (if (.task_indicator.completed != null and .task_indicator.total != null)
          then "Tasks \(.task_indicator.completed)/\(.task_indicator.total)"
          else "" end))
     else "" end) as $task_text |
    [
        (.workspace.current_dir // .cwd // ""),
        (.harness // ""),
        (.session_id // ""),
        (.model.id // "unknown"),
        $ctx_pct,
        $ctx_used,
        $ctx_size,
        (.session_name // ""),
        (.vim.mode // ""),
        $task_text,
        (.context_window.total_input_tokens // 0),
        (.context_window.total_output_tokens // 0),
        (.cost.total_cost_usd // ""),
        (.cost.total_lines_added // 0),
        (.cost.total_lines_removed // 0),
        (.cost.total_duration_ms // ""),
        (.version // "")
    ]
    | map(if . == null then "" else tostring end | gsub("[\r\n\t\u001f]+"; " "))
    | join("\u001f")
')

IFS=$'\037' read -r cwd harness session_id model ctx_pct ctx_used ctx_size session_name vim_mode task_text in_tok out_tok cost added removed dur_ms version <<< "$status_fields"

# Helpers
fmt_duration() {
    local ms=$1 s m h
    s=$((ms / 1000))
    h=$((s / 3600)); m=$(( (s % 3600) / 60 )); s=$((s % 60))
    if [ $h -gt 0 ]; then
        printf '%dh%dm' "$h" "$m"
    elif [ $m -gt 0 ]; then
        printf '%dm%ds' "$m" "$s"
    else
        printf '%ds' "$s"
    fi
}

fmt_tokens_var() {
    local __var=$1
    local t=${2:-0}
    [[ "$t" =~ ^[0-9]+$ ]] || t=0
    if [ "$t" -ge 1000000 ]; then
        printf -v "$__var" '%d.%dM' "$((t / 1000000))" "$(((t % 1000000) / 100000))"
    elif [ "$t" -ge 1000 ]; then
        printf -v "$__var" '%d.%dk' "$((t / 1000))" "$(((t % 1000) / 100))"
    else
        printf -v "$__var" '%d' "$t"
    fi
}

is_nonzero_number() {
    local value=${1:-}
    local normalized
    [ -n "$value" ] && [ "$value" != "null" ] || return 1
    normalized=$(printf '%.6f' "$value" 2>/dev/null) || return 1
    [ "$normalized" != "0.000000" ]
}

# Separator (dark gray dot)
sep="\033[38;5;240m•\033[0m"

# Collect visible parts — separators are only rendered between non-empty entries
parts=()

# Detect worktree once (used by both DIR and GIT sections)
is_git_repo=0
git_dir=""
git_common_dir=""
if git_meta=$(git -C "$cwd" rev-parse --path-format=absolute --absolute-git-dir --git-common-dir 2>/dev/null); then
    is_git_repo=1
    git_dir=${git_meta%%$'\n'*}
    git_common_dir=${git_meta#*$'\n'}
    git_common_dir=${git_common_dir%%$'\n'*}
fi
if [ "$is_git_repo" = "1" ] && [ -n "$git_dir" ] && [ -n "$git_common_dir" ] && [ "$git_dir" != "$git_common_dir" ]; then
    is_worktree=1
else
    is_worktree=0
fi

# --- Directory (blue) ---
if [ "$ENABLE_DIR" = "1" ]; then
    repo_root="$cwd"
    if [ -n "$git_common_dir" ]; then
        case "$git_common_dir" in
            */.git) repo_root=${git_common_dir%/.git} ;;
            *) repo_root=$git_common_dir ;;
        esac
    fi
    dir=${repo_root:-$cwd}
    dir=${dir%/}
    dir=${dir##*/}
    if [ -n "$dir" ]; then
        if [ "$is_worktree" = "1" ]; then
            parts+=("\033[34m${dir}\033[38;5;215m⧉\033[0m")
        else
            parts+=("\033[34m${dir}\033[0m")
        fi
    fi
fi

# --- Git info (green) ---
if [ "$ENABLE_GIT" = "1" ] && [ "$is_git_repo" = "1" ]; then
    git_status=$(git -C "$cwd" --no-optional-locks status --porcelain=v1 --branch -uno 2>/dev/null)
    first_git_status_line=${git_status%%$'\n'*}
    branch=${first_git_status_line#'## '}
    branch=${branch%%...*}
    branch=${branch%% \[*}
    [ "$branch" = "HEAD (no branch)" ] && branch="HEAD"
    if [ "$git_status" != "$first_git_status_line" ]; then
        dirty="*"
    else
        dirty=""
    fi
    git_info="${branch}${dirty}"
    [ -n "$git_info" ] && parts+=("\033[32m${git_info}\033[0m")
fi

# --- Codex session name (lavender) ---
if [ "$ENABLE_SESSION_NAME" = "1" ] && [ "$harness" = "codex" ]; then
    [ -n "$session_name" ] && parts+=("\033[38;5;141m${session_name}\033[0m")
fi

# --- Model (magenta) ---
if [ "$ENABLE_MODEL" = "1" ] && [ -n "$model" ] && [ "$model" != "unknown" ]; then
    parts+=("\033[35m${model}\033[0m")
fi

# --- Vim mode (bold yellow) ---
if [ "$ENABLE_VIM_MODE" = "1" ]; then
    [ -n "$vim_mode" ] && parts+=("\033[1;33m${vim_mode}\033[0m")
fi

# --- Context usage (cyan) ---
if [ "$ENABLE_CONTEXT" = "1" ] && [ -n "$ctx_pct" ]; then
    context_info="${ctx_pct}%"
    if [ "$ENABLE_CONTEXT_TOKENS" = "1" ] && [ "${ctx_size:-0}" -gt 0 ] 2>/dev/null; then
        ctx_used_fmt=""
        ctx_size_fmt=""
        fmt_tokens_var ctx_used_fmt "${ctx_used:-0}"
        fmt_tokens_var ctx_size_fmt "$ctx_size"
        context_info+=" ${ctx_used_fmt}/${ctx_size_fmt}"
    fi
    parts+=("\033[36m${context_info}\033[0m")
fi

# --- Codex task indicator (orange) ---
if [ "$ENABLE_TASK_INDICATOR" = "1" ] && [ "$harness" = "codex" ]; then
    [ -n "$task_text" ] && parts+=("\033[38;5;208m${task_text}\033[0m")
fi

# --- Token counts (dim) ---
if [ "$ENABLE_TOKENS" = "1" ]; then
    if [ "$in_tok" != "0" ] || [ "$out_tok" != "0" ]; then
        in_tok_fmt=""
        out_tok_fmt=""
        fmt_tokens_var in_tok_fmt "$in_tok"
        fmt_tokens_var out_tok_fmt "$out_tok"
        parts+=("\033[2m${in_tok_fmt}\xe2\x86\x93${out_tok_fmt}\xe2\x86\x91\033[0m")
    fi
fi

# --- Cost (yellow) ---
if [ "$ENABLE_COST" = "1" ]; then
    if is_nonzero_number "$cost"; then
        printf -v cost_text '$%.2f' "$cost"
        parts+=("\033[33m${cost_text}\033[0m")
    fi
fi

# --- Lines changed (green +N / red -N) ---
if [ "$ENABLE_LINES" = "1" ]; then
    if [ "$added" != "0" ] || [ "$removed" != "0" ]; then
        parts+=("\033[32m+${added}\033[0m/\033[31m-${removed}\033[0m")
    fi
fi

# --- Session duration (white) ---
if [ "$ENABLE_DURATION" = "1" ]; then
    if [ -n "$dur_ms" ] && [ "$dur_ms" != "0" ]; then
        parts+=("\033[37m$(fmt_duration "$dur_ms")\033[0m")
    fi
fi

# --- Version (dim gray) ---
if [ "$ENABLE_VERSION" = "1" ]; then
    [ -n "$version" ] && parts+=("\033[2;37mv${version}\033[0m")
fi

# --- Join parts with separator (only between non-empty elements) ---
output=""
for i in "${!parts[@]}"; do
    [ "$i" -gt 0 ] && output+=" $sep "
    output+="${parts[$i]}"
done

printf '%b' "$output"
