# App manual checklist (Step 83)

1. **Create VM** — New VM → name `apptest` → Create. Entry appears in the sidebar.
2. **Start** — Point `kernel` in config at a real `Image` if needed, then Start. State should reach **Running** within ~2s (500ms status polls).
3. **Logs** — Host log panel scrolls as `ternvale-apptest-*.log` grows.
4. **Stop** — Stop → **Stopping…** then **Stopped**; socket gone.
5. **Crash** — Start again, `kill -9` the `ternvale run` helper (Activity Monitor or `pkill -9 -f "ternvale run"`). UI should show **Crashed** with a summary from `.summary.txt` / `.run.json` when present, not hang on Starting/Running.
