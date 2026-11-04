-- Plays every unique demo below a navigator path and sorts them into playlists
-- by what they did. Demos already on one of those playlists are skipped. Run
-- with no files, so the default dbs are loaded:
--
--   demarc --headless --no-silence --remote-control scripts/test_demos.lua
--
-- Each demo prints a RESULT line and leaves a screenshot in SHOTS. A demo that
-- is silent but moving counts as working, and is marked "silent" in its line.
-- One with audio and a picture that never moves goes on the black screen list:
-- that is how a demo looks when its frames don't reach the session.

-- "*" in a segment matches any text.
local PATH = "Demozoo/Awards/Scene.org *"
local SHOTS = "test_shots/"

local NO_START = "Test: No Start"
local BLACK = "Test: Black Screen"
local WORKING = "Test: Working"

local LOAD_TIMEOUT = 300
local RUN_TIME = 60
-- Seconds to keep looking once a demo has shown both sound and moving video.
local MIN_RUN_TIME = 10
local SAMPLE_FRAMES = 30
local AUDIO_MIN = 0.001
local LIT_MIN = 0.002
-- Distinct frames that count as moving; a blinking prompt makes two.
local MOVING_FRAMES = 4

local function wait_loaded()
    local start = os.time()
    wait_frames(2)
    while os.time() - start < LOAD_TIMEOUT do
        local status, err = load_status()
        if status ~= "loading" then
            return status, err
        end
        wait_frames(SAMPLE_FRAMES)
    end
    return "timeout", "still loading after " .. LOAD_TIMEOUT .. "s"
end

local function watch()
    local start = os.time()
    local heard, lit = false, false
    local seen, frames = {}, 0
    audio_level()
    repeat
        wait_frames(SAMPLE_FRAMES)
        heard = heard or audio_level() > AUDIO_MIN
        local level, hash = video_level()
        if level > LIT_MIN then
            lit = true
            if not seen[hash] then
                seen[hash] = true
                frames += 1
            end
        end
        local elapsed = os.time() - start
        local done = heard and frames >= MOVING_FRAMES and elapsed >= MIN_RUN_TIME
    until done or elapsed >= RUN_TIME
    return heard, lit, frames >= MOVING_FRAMES
end

local function test_demo(demo)
    load_demo(demo.id)
    local status, err = wait_loaded()
    if status ~= "running" then
        return NO_START, status .. ": " .. tostring(err)
    end
    local heard, lit, moving = watch()
    screenshot(SHOTS .. demo.id .. ".png")
    if heard then
        if moving then
            return WORKING, ""
        end
        return BLACK, lit and "audio, still picture" or "audio only"
    end
    if moving then
        return WORKING, "silent"
    end
    return NO_START, lit and "silent still picture" or "silent and black"
end

function Main()
    local demos, seen = {}, {}
    for _, list in { NO_START, BLACK, WORKING } do
        for _, demo in get_playlist(list) or {} do
            seen[demo.id] = true
        end
    end
    for _, demo in collect_navigator(PATH) do
        if not seen[demo.id] then
            seen[demo.id] = true
            table.insert(demos, demo)
        end
    end
    if #demos == 0 then
        error("nothing left to test at " .. PATH)
    end
    print("Testing " .. #demos .. " demos from " .. PATH)

    local counts = { [NO_START] = 0, [BLACK] = 0, [WORKING] = 0 }
    for i, demo in demos do
        print(string.format("[%d/%d] %s %s", i, #demos, demo.id, demo.title or "?"))
        local result, note = test_demo(demo)
        for list in counts do
            if list ~= result then
                remove_from_playlist(list, demo.id)
            end
        end
        save_to_playlist(result, demo.id)
        counts[result] += 1
        print(string.format("RESULT\t%s\t%s\t%s\t%s", result, demo.id, demo.title or "?", note))
    end

    for list, count in counts do
        print(list .. ": " .. count)
    end
    wait_frames(2)
    quit()
end
