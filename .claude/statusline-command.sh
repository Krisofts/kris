#!/bin/bash
# KRIS status line: project name | git branch | worktree (or working directory)
set -u

input=$(cat)

cwd=$(printf '%s' "$input" | jq -r '.workspace.current_dir // .cwd // empty' 2>/dev/null)
project_dir=$(printf '%s' "$input" | jq -r '.workspace.project_dir // empty' 2>/dev/null)
repo_owner=$(printf '%s' "$input" | jq -r '.workspace.repo.owner // empty' 2>/dev/null)
repo_name=$(printf '%s' "$input" | jq -r '.workspace.repo.name // empty' 2>/dev/null)
worktree_name=$(printf '%s' "$input" | jq -r '.worktree.name // empty' 2>/dev/null)
git_worktree=$(printf '%s' "$input" | jq -r '.workspace.git_worktree // empty' 2>/dev/null)

dir="${project_dir:-${cwd:-$(pwd)}}"

# Project name: prefer "owner/name" from the repo's origin remote, else the directory's basename.
if [ -n "$repo_name" ]; then
  if [ -n "$repo_owner" ]; then
    project_name="${repo_owner}/${repo_name}"
  else
    project_name="$repo_name"
  fi
else
  project_name=$(basename "$dir")
fi

# Git branch: robust to detached HEAD, unborn HEAD (no commits yet), and non-git directories.
branch=""
if git --no-optional-locks -C "$dir" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  branch=$(git --no-optional-locks -C "$dir" symbolic-ref --quiet --short HEAD 2>/dev/null)
  if [ -z "$branch" ]; then
    sha=$(git --no-optional-locks -C "$dir" rev-parse --short HEAD 2>/dev/null)
    if [ -n "$sha" ]; then
      branch="detached@${sha}"
    else
      branch="no-commits"
    fi
  fi
else
  branch="no-git"
fi

# Worktree: show the linked worktree name/path if this session is running in one,
# else fall back to the working directory.
if [ -n "$worktree_name" ]; then
  wt="worktree:${worktree_name}"
elif [ -n "$git_worktree" ]; then
  wt="worktree:${git_worktree}"
else
  wt="$dir"
fi

printf '\033[2;36m%s\033[0m \033[2m|\033[0m \033[2;32mbranch:%s\033[0m \033[2m|\033[0m \033[2;33m%s\033[0m\n' \
  "$project_name" "$branch" "$wt"
