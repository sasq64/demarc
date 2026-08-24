#!/usr/bin/env python3
"""Drive a libretro core headlessly, straight from Python, with no Bevy.

demarc's own core plumbing (src/retro_emu.rs) is the wrong tool for asking
"does this core boot at all" -- a failure there could be the core, the
threading, the Bevy plugin or the renderer. This implements just enough of a
libretro frontend to answer the question on its own: load the core, hand it
some content, step it, and write out the last frame plus the audio tally.

    scripts/libretro-smoke.py external/pcem/build-lr/src/pcem_libretro.so \\
        machine.cfg --system-dir ~/.cache/demarc/system --frames 180 --png frame.png

The environment implementation deliberately declines GET_LOG_INTERFACE:
retro_log_printf_t is variadic, which ctypes cannot express as a callback, so
we would only ever see the format string. Declining makes well-behaved cores
fall back to writing their own messages to stderr, which is what we want to
read here.
"""

import argparse
import ctypes as C
import os
import sys

# retro_environment commands, from libretro.h.
SET_MESSAGE = 6
SHUTDOWN = 7
GET_SYSTEM_DIRECTORY = 9
SET_PIXEL_FORMAT = 10
SET_KEYBOARD_CALLBACK = 12
GET_VARIABLE = 15
SET_VARIABLES = 16
GET_VARIABLE_UPDATE = 17
SET_SUPPORT_NO_GAME = 18
GET_LOG_INTERFACE = 27
GET_SAVE_DIRECTORY = 31
SET_SYSTEM_AV_INFO = 32
SET_GEOMETRY = 37
GET_CORE_OPTIONS_VERSION = 52

PIXEL_FORMATS = {0: "0RGB1555", 1: "XRGB8888", 2: "RGB565"}


class GameInfo(C.Structure):
    _fields_ = [
        ("path", C.c_char_p),
        ("data", C.c_void_p),
        ("size", C.c_size_t),
        ("meta", C.c_char_p),
    ]


class Geometry(C.Structure):
    _fields_ = [
        ("base_width", C.c_uint),
        ("base_height", C.c_uint),
        ("max_width", C.c_uint),
        ("max_height", C.c_uint),
        ("aspect_ratio", C.c_float),
    ]


class Timing(C.Structure):
    _fields_ = [("fps", C.c_double), ("sample_rate", C.c_double)]


class AVInfo(C.Structure):
    _fields_ = [("geometry", Geometry), ("timing", Timing)]


class SystemInfo(C.Structure):
    _fields_ = [
        ("library_name", C.c_char_p),
        ("library_version", C.c_char_p),
        ("valid_extensions", C.c_char_p),
        ("need_fullpath", C.c_bool),
        ("block_extract", C.c_bool),
    ]


class Variable(C.Structure):
    _fields_ = [("key", C.c_char_p), ("value", C.c_char_p)]


def parse_args():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("core", help="path to the libretro dynamic library")
    ap.add_argument("content", nargs="?", help="content path (omit for a no-game core)")
    ap.add_argument("--system-dir", default=".", help="GET_SYSTEM_DIRECTORY")
    ap.add_argument("--save-dir", help="GET_SAVE_DIRECTORY (defaults to --system-dir)")
    ap.add_argument("--frames", type=int, default=120, help="retro_run calls")
    ap.add_argument("--png", help="write the last frame here (needs Pillow)")
    ap.add_argument("--reset-at", type=int, help="call retro_reset at this frame")
    ap.add_argument("-O", "--option", action="append", default=[], metavar="KEY=VALUE",
                    help="override a core option (repeatable)")
    return ap.parse_args()


def main():
    args = parse_args()
    overrides = dict(o.split("=", 1) for o in args.option)

    system_dir = C.c_char_p(os.path.abspath(args.system_dir).encode())
    save_dir = C.c_char_p(os.path.abspath(args.save_dir or args.system_dir).encode())

    core = C.CDLL(args.core)
    state = {"shutdown": False, "geom": None, "frame": None, "audio": 0, "options": {}}
    # ctypes does not keep buffers handed to C alive on its own.
    keepalive = []

    @C.CFUNCTYPE(C.c_bool, C.c_uint, C.c_void_p)
    def environment(cmd, data):
        if cmd == GET_SYSTEM_DIRECTORY:
            C.cast(data, C.POINTER(C.c_char_p))[0] = system_dir
            return True
        if cmd == GET_SAVE_DIRECTORY:
            C.cast(data, C.POINTER(C.c_char_p))[0] = save_dir
            return True
        if cmd == GET_LOG_INTERFACE:
            return False  # see the module docstring
        if cmd == SET_PIXEL_FORMAT:
            fmt = C.cast(data, C.POINTER(C.c_int))[0]
            print(f"pixel format: {PIXEL_FORMATS.get(fmt, fmt)}")
            state["pixel_format"] = fmt
            return fmt in PIXEL_FORMATS
        if cmd == SET_VARIABLES:
            arr = C.cast(data, C.POINTER(Variable))
            i = 0
            while arr[i].key:
                key = arr[i].key.decode()
                default = arr[i].value.decode().split(";", 1)[1].strip().split("|")[0]
                state["options"][key] = overrides.get(key, default)
                i += 1
            print("core options:", state["options"])
            return True
        if cmd == GET_VARIABLE:
            var = C.cast(data, C.POINTER(Variable))
            value = state["options"].get(var[0].key.decode())
            if value is None:
                return False
            buf = C.c_char_p(value.encode())
            keepalive.append(buf)
            var[0].value = buf
            return True
        if cmd == GET_VARIABLE_UPDATE:
            C.cast(data, C.POINTER(C.c_bool))[0] = False
            return True
        if cmd == SET_GEOMETRY:
            g = C.cast(data, C.POINTER(Geometry))[0]
            state["geom"] = (g.base_width, g.base_height, round(g.aspect_ratio, 4))
            return True
        if cmd == SET_SYSTEM_AV_INFO:
            av = C.cast(data, C.POINTER(AVInfo))[0]
            print(f"core changed av info: fps {av.timing.fps} rate {av.timing.sample_rate}")
            return True
        if cmd == SHUTDOWN:
            state["shutdown"] = True
            return True
        if cmd == GET_CORE_OPTIONS_VERSION:
            # demarc answers 0, forcing the legacy SET_VARIABLES path. Match it,
            # so a core is exercised here the same way demarc will exercise it.
            C.cast(data, C.POINTER(C.c_uint))[0] = 0
            return True
        if cmd == SET_MESSAGE:
            class Message(C.Structure):
                _fields_ = [("msg", C.c_char_p), ("frames", C.c_uint)]
            print("core message:", C.cast(data, C.POINTER(Message))[0].msg.decode())
            return True
        if cmd in (SET_KEYBOARD_CALLBACK, SET_SUPPORT_NO_GAME):
            return True
        print(f"unhandled environment command {cmd}")
        return False

    @C.CFUNCTYPE(None, C.c_void_p, C.c_uint, C.c_uint, C.c_size_t)
    def video_refresh(data, width, height, pitch):
        if data:  # a null frame is a dupe of the last one
            state["frame"] = (C.string_at(data, pitch * height), width, height, pitch)

    @C.CFUNCTYPE(C.c_size_t, C.POINTER(C.c_int16), C.c_size_t)
    def audio_batch(data, frames):
        state["audio"] += frames
        return frames

    @C.CFUNCTYPE(None, C.c_int16, C.c_int16)
    def audio_sample(left, right):
        state["audio"] += 1

    @C.CFUNCTYPE(None)
    def input_poll():
        pass

    @C.CFUNCTYPE(C.c_int16, C.c_uint, C.c_uint, C.c_uint, C.c_uint)
    def input_state(port, device, index, ident):
        return 0

    keepalive += [environment, video_refresh, audio_batch, audio_sample, input_poll, input_state]
    core.retro_set_environment(environment)
    core.retro_set_video_refresh(video_refresh)
    core.retro_set_audio_sample_batch(audio_batch)
    core.retro_set_audio_sample(audio_sample)
    core.retro_set_input_poll(input_poll)
    core.retro_set_input_state(input_state)

    core.retro_api_version.restype = C.c_uint
    print("libretro api version:", core.retro_api_version())

    info = SystemInfo()
    core.retro_get_system_info(C.byref(info))
    print(f"core: {info.library_name.decode()} {info.library_version.decode()} "
          f"extensions={info.valid_extensions.decode()} need_fullpath={info.need_fullpath}")

    core.retro_init()

    core.retro_load_game.restype = C.c_bool
    if args.content:
        game = GameInfo(path=os.path.abspath(args.content).encode(), data=None, size=0, meta=None)
        loaded = core.retro_load_game(C.byref(game))
    else:
        loaded = core.retro_load_game(None)
    if not loaded:
        print("retro_load_game failed", file=sys.stderr)
        core.retro_deinit()
        return 2

    av = AVInfo()
    core.retro_get_system_av_info(C.byref(av))
    print(f"av: {av.geometry.base_width}x{av.geometry.base_height} "
          f"(max {av.geometry.max_width}x{av.geometry.max_height}) "
          f"aspect {av.geometry.aspect_ratio:.4f} "
          f"fps {av.timing.fps} rate {av.timing.sample_rate}")

    for frame in range(args.frames):
        core.retro_run()
        if frame == args.reset_at:
            print(f"retro_reset at frame {frame}")
            core.retro_reset()
        if state["shutdown"]:
            print(f"core requested shutdown at frame {frame}")
            break

    if state["geom"]:
        print("geometry after SET_GEOMETRY:", state["geom"])
    expected = int(args.frames * av.timing.sample_rate / av.timing.fps)
    print(f"audio frames: {state['audio']} (one frame's worth per tick would be {expected})")

    if state["frame"]:
        buf, width, height, pitch = state["frame"]
        print(f"last frame: {width}x{height} pitch {pitch}")
        if args.png:
            write_png(args.png, buf, width, height, pitch, state.get("pixel_format"))
    else:
        print("no frame was produced")

    core.retro_unload_game()
    core.retro_deinit()
    print("clean shutdown")
    return 0


def write_png(path, buf, width, height, pitch, pixel_format):
    try:
        from PIL import Image
    except ImportError:
        print("Pillow is not installed; skipping the PNG", file=sys.stderr)
        return
    if pixel_format == 1:  # XRGB8888, little-endian, so BGRA in memory
        img = Image.frombuffer("RGBA", (pitch // 4, height), buf, "raw", "BGRA", 0, 1)
    elif pixel_format == 2:  # RGB565
        img = Image.frombuffer("RGB", (pitch // 2, height), buf, "raw", "BGR;16", 0, 1)
    else:
        print(f"cannot write a PNG for pixel format {pixel_format}", file=sys.stderr)
        return
    img.crop((0, 0, width, height)).convert("RGB").save(path)
    print(f"wrote {path}")


if __name__ == "__main__":
    sys.exit(main())
