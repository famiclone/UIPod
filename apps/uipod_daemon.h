/***************************************************************************
 * UIPod: NextUI "Minimize to Background" Daemon & Unix IPC (`/tmp/uipod.sock`).
 *
 * Supports:
 * - Fast `--client` thin blocker mode (cold-spawns detached `--daemon` via
 *   `fork()` + `setsid()`, sends `ATTACH_UI\n`, and waits for `DETACHED\n` or
 *   `EXITED\n` so NextUI reclaims the display & gamepad seamlessly).
 * - Zero-glitch SDL2/GLES3 teardown & re-attach (`MENU + START` or menu row)
 *   while ALSA `Playback` (`dmix`) continues uninterrupted in the background.
 * - Non-exclusive `/dev/input/event*` (`O_RDONLY | O_NONBLOCK | O_CLOEXEC`,
 *   never `EVIOCGRAB`) background media chords (`MENU + R2/L2/START/Up/Down`).
 * - 15-minute idle/paused forgotten-daemon battery guard & clean `Quit UIPod`.
 ****************************************************************************/
#ifndef _UIPOD_DAEMON_H
#define _UIPOD_DAEMON_H

#include <stdbool.h>
#include <stddef.h>

/* Invoked at the very top of `main(argc, argv)` BEFORE SDL or Rockbox init.
 * - If `--client` is passed: attaches to (or cold-spawns) `--daemon`, blocks
 *   until `DETACHED` or `EXITED`, and terminates immediately via `_exit(0)`.
 * - If `--daemon` is passed: binds `/tmp/uipod.sock`, writes `/tmp/uipod.pid`,
 *   and configures shared ALSA `dmix` mode. */
void uipod_daemon_early_main(int argc, char *argv[]);

/* Called once after `pcm_init()` / `settings_apply()` so `--daemon` forces
 * shared ALSA `Playback` (`dmix`) from frame 0 (zero-click minimize/re-attach). */
void uipod_daemon_post_audio_init(void);

/* Returns true when running on Linux handheld (`tg5040` / NextUI) where
 * Minimize to Background is supported. */
bool uipod_daemon_supported(void);

/* Requests immediate transition into headless background daemon mode. */
void uipod_daemon_request_minimize(void);

/* Returns true if a minimize request (via `MENU + START` or menu item) is pending. */
bool uipod_daemon_minimize_pending(void);

/* Polls `SIGTERM`/`SIGINT` quit requests and executes the full SDL teardown ->
 * headless background loop -> SDL re-attach cycle if minimize was requested
 * (when `allow_minimize == true`). Returns `true` if the UI just re-attached
 * from background mode and the caller must immediately repaint its screen! */
bool uipod_daemon_poll(bool allow_minimize);

/* Cleanly stops playback, flushes settings/resume/scrobblers, notifies any
 * attached `--client` (`EXITED\n`), unlinks `/tmp/uipod.sock` & `/tmp/uipod.pid`,
 * and shuts down the daemon process. */
void uipod_daemon_quit(void);

/* Notifies any attached `--client` (`EXITED\n`) and unlinks `/tmp/uipod.sock`
 * & `/tmp/uipod.pid` during system poweroff/reboot (`clean_shutdown`). */
void uipod_daemon_notify_exit(void);

/* Localized (`EN` / `UA`) dynamic labels for Main Menu & Now Playing menus. */
char *uipod_daemon_minimize_label(int selected_item, void *data,
                                  char *buffer, size_t buffer_len);
char *uipod_daemon_quit_label(int selected_item, void *data,
                              char *buffer, size_t buffer_len);

#endif /* _UIPOD_DAEMON_H */
