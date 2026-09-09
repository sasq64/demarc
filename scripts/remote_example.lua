-- Example for --remote-control: shoot the first demo, skip to the next, shoot
-- that one, quit. Globals come from src/remote_control.rs; Key.* and Cmd.*
-- name every key the emulator understands and every hotkey command.

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
