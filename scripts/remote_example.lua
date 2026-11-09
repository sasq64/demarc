-- Example for --remote-control: shoot the first demo, skip to the next, shoot
-- that one, quit. Globals come from src/remote_control.rs; Key.* and Cmd.*
-- name every key the emulator understands and every hotkey command.
--
-- load_demo(id) starts an entry by id: "demozoo.1234", "csdb.5678", or a bare id
-- in the first db. load_status() then gives "loading", "running", "idle", or
-- "failed" plus the reason.
-- get_navigator(path) lists the entries at a navigator path like
-- "Demozoo/Parties/Evoke 2005/Demo", as tables of the db's fields with the id in
-- the form above; nil if there is no such path. collect_navigator(pattern) gives
-- every entry in the folders below a path, where a "*" in a segment matches any
-- text: "Demozoo/Awards/Scene.org *". get_playlist(name) lists a playlist, and
-- save_to_playlist(name, id) and remove_from_playlist(name, id) change one,
-- creating it if needed.
-- audio_level() is the RMS (0..1) of what the cores played since it was last
-- asked, and record_audio("out.wav") writes all of it from then on.
-- video_level() is the share (0..1) of the current frame that isn't black, plus
-- a hash that tells frames apart.
-- screenshot(path) lands a few frames later; quit() waits for it. It shows
-- everything on screen, the picker and dialogs too, headless or not.
-- memory() is the resident and peak resident size of the process in KB.

function Main()
    wait_frames(300)
    screenshot("shot1.png")

    press_key(Key.Space)
    wait_frames(300)

    send_cmd(Cmd.NextFile)
    wait_frames(300)
    screenshot("shot2.png")

    wait_frames(10)
    quit()
end
