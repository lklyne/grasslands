# 0013: File tree search and drag to move

- **Search runs in-process** with `ignore`'s walker and a plain substring match (smart case), not ripgrep as a subprocess: no extra binary to find, and the same ignore rules as the tree. It runs on the background executor after a 120 ms pause in typing; a newer query sets the old one's cancel flag. Caps (100 name hits, 500 line hits, 20 per file, files over 1 MiB and binary files skipped) keep a one-letter query responsive. Results re-run when the watcher reports changes.
- **Results replace the tree** while the query is non-empty, instead of filtering the tree in place: the Kit tree is lazily loaded, so filtering it would mean loading everything anyway, and content hits need their own rows.
- **Line hits open via `OpenFileAt`**, a separate action from `OpenFile`, so existing senders are unchanged.
- **Drag to move** uses GPUI's `on_drag`/`on_drop` on the Kit's `ListItem` rows. A drop on a file targets its folder; a drop anywhere else in the panel targets the root. The Kit tree toggles a folder on mouse down, so dragging a folder also folds/unfolds it; that's the Kit's behaviour and not worth forking for.
