/* C API over LinuxCNC's NML status, command and error channels, for the
 * `lcnc` Rust crate.
 *
 * It copies EMC_STAT into a flat struct, so Rust never depends on the C++
 * class layout. Not thread safe: one lcnc_t is used from one thread at a time.
 */
#pragma once
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define LCNC_AXES 9      /* X Y Z A B C U V W */
#define LCNC_JOINTS 16
#define LCNC_GCODES 17
#define LCNC_MCODES 10
#define LCNC_SETTINGS 5
#define LCNC_PATH 256

typedef struct lcnc lcnc_t;

typedef struct {
    /* task */
    int32_t task_mode;      /* 1 manual, 2 auto, 3 mdi */
    int32_t task_state;     /* 1 estop, 2 estop reset, 3 off, 4 on */
    int32_t exec_state;
    int32_t interp_state;   /* 1 idle, 2 reading, 3 paused, 4 waiting */
    int32_t call_level;
    int32_t motion_line;
    int32_t current_line;
    int32_t read_line;
    int32_t optional_stop;
    int32_t block_delete;
    int32_t task_paused;
    int32_t queued_mdi;
    int32_t g5x_index;      /* 1 = G54 ... 9 = G59.3 */
    int32_t program_units;  /* 1 inch, 2 mm, 3 cm */
    int32_t interp_errcode;
    double delay_left;
    double rotation_xy;
    double g5x_offset[LCNC_AXES];
    double g92_offset[LCNC_AXES];
    double tool_offset[LCNC_AXES];
    int32_t active_gcodes[LCNC_GCODES];
    int32_t active_mcodes[LCNC_MCODES];
    double active_settings[LCNC_SETTINGS];
    char file[LCNC_PATH];
    char command[LCNC_PATH];
    char ini_filename[LCNC_PATH];

    /* trajectory */
    int32_t joints;
    int32_t axis_mask;
    int32_t traj_mode;      /* 1 free, 2 coord, 3 teleop */
    int32_t enabled;
    int32_t inpos;
    int32_t queue;
    int32_t paused;
    int32_t probe_tripped;
    int32_t probing;
    int32_t probeval;
    int32_t motion_type;
    int32_t feed_hold_enabled;
    int32_t feed_override_enabled;
    double linear_units;
    double angular_units;
    double cycle_time;
    double feed_scale;
    double rapid_scale;
    double current_vel;
    double distance_to_go;
    double max_velocity;
    double position[LCNC_AXES];        /* commanded, machine coords */
    double actual_position[LCNC_AXES]; /* feedback, machine coords */
    double dtg[LCNC_AXES];
    double probed_position[LCNC_AXES];

    /* joints */
    uint8_t homed[LCNC_JOINTS];
    uint8_t homing[LCNC_JOINTS];
    uint8_t fault[LCNC_JOINTS];
    uint8_t min_soft_limit[LCNC_JOINTS];
    uint8_t max_soft_limit[LCNC_JOINTS];
    uint8_t min_hard_limit[LCNC_JOINTS];
    uint8_t max_hard_limit[LCNC_JOINTS];
    double ferror[LCNC_JOINTS];
    int32_t on_soft_limit;

    /* spindle 0 */
    double spindle_speed;      /* commanded rpm */
    double spindle_scale;
    int32_t spindle_direction; /* -1, 0, 1 */
    int32_t spindle_enabled;
    int32_t spindle_brake;
    int32_t spindle_override_enabled;

    /* io */
    int32_t estop;
    int32_t tool_in_spindle;
    int32_t pocket_prepped;
    int32_t mist;
    int32_t flood;

    /* last command echo */
    int32_t echo_serial;
    int32_t command_status; /* 1 done, 2 exec, 3 error */
} lcnc_status_t;

/* sizeof(lcnc_status_t), checked by Rust against its mirror of the struct. */
size_t lcnc_status_size(void);

/* Connect to a running LinuxCNC. nml_file NULL uses the default linuxcnc.nml.
 * Returns NULL and fills err on failure. */
lcnc_t *lcnc_open(const char *nml_file, char *err, size_t err_len);
void lcnc_close(lcnc_t *c);

/* Copy the newest status into out. Returns 1 when it was copied, 0 when
 * nothing changed since the last call that returned 1 (out is left as it
 * was), -1 if the channel is invalid or no status has arrived yet. */
int lcnc_poll(lcnc_t *c, lcnc_status_t *out);

/* Next operator message, if any. Returns 0 when there is none, else the kind
 * (1 error, 2 text, 3 display) with the message copied into buf. -1 on failure. */
int lcnc_next_error(lcnc_t *c, char *buf, size_t len);

/* Commands. Each returns the command serial number (>0) or -1 if the write
 * failed. Completion shows up in lcnc_status_t.echo_serial/command_status. */
int lcnc_set_state(lcnc_t *c, int state);
int lcnc_set_mode(lcnc_t *c, int mode);
int lcnc_abort(lcnc_t *c);
int lcnc_mdi(lcnc_t *c, const char *line);
int lcnc_program_open(lcnc_t *c, const char *path);
int lcnc_program_run(lcnc_t *c, int line);
int lcnc_program_pause(lcnc_t *c);
int lcnc_program_resume(lcnc_t *c);
int lcnc_program_step(lcnc_t *c);
int lcnc_set_optional_stop(lcnc_t *c, int on);
int lcnc_set_block_delete(lcnc_t *c, int on);
int lcnc_home(lcnc_t *c, int joint); /* -1 homes all */
int lcnc_unhome(lcnc_t *c, int joint);
int lcnc_teleop(lcnc_t *c, int on);
/* joint_mode 1 jogs a joint, 0 jogs an axis (0 = X, 1 = Y, ...). */
int lcnc_jog_cont(lcnc_t *c, int joint_mode, int index, double vel);
int lcnc_jog_incr(lcnc_t *c, int joint_mode, int index, double vel, double incr);
int lcnc_jog_stop(lcnc_t *c, int joint_mode, int index);
int lcnc_feed_scale(lcnc_t *c, double scale);
int lcnc_rapid_scale(lcnc_t *c, double scale);
int lcnc_spindle_scale(lcnc_t *c, double scale);
int lcnc_spindle_on(lcnc_t *c, double rpm); /* negative rpm runs CCW */
int lcnc_spindle_off(lcnc_t *c);
int lcnc_spindle_brake(lcnc_t *c, int engage);
int lcnc_max_velocity(lcnc_t *c, double vel);
int lcnc_feed_override_enable(lcnc_t *c, int on);
int lcnc_spindle_override_enable(lcnc_t *c, int on);
int lcnc_feed_hold_enable(lcnc_t *c, int on);
int lcnc_coolant(lcnc_t *c, int flood, int on);
int lcnc_load_tool_table(lcnc_t *c);
int lcnc_task_plan_synch(lcnc_t *c);

#ifdef __cplusplus
}
#endif
