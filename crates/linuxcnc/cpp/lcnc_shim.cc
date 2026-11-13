// See lcnc_shim.h. Modeled on LinuxCNC's emcmodule.cc and shcom.cc.
#include "lcnc_shim.h"

#include <emc.hh>
#include <emc_nml.hh>
#include <emccfg.h>
#include <nml_oi.hh>
#include <rcs.hh>
#include <rcs_print.hh>

#include <stdio.h>
#include <string.h>

struct lcnc {
    RCS_STAT_CHANNEL *stat;
    RCS_CMD_CHANNEL *cmd;
    NML *err;
    int serial;
    bool have_status; // peek() returns 0 when nothing changed since the last peek
};

static void copy_str(char *dst, size_t len, const char *src) {
    if (!len) return;
    strncpy(dst, src, len - 1);
    dst[len - 1] = 0;
}

static void pose(double out[LCNC_AXES], const EmcPose &p) {
    out[0] = p.tran.x; out[1] = p.tran.y; out[2] = p.tran.z;
    out[3] = p.a; out[4] = p.b; out[5] = p.c;
    out[6] = p.u; out[7] = p.v; out[8] = p.w;
}

extern "C" size_t lcnc_status_size(void) { return sizeof(lcnc_status_t); }

extern "C" lcnc_t *lcnc_open(const char *nml_file, char *err, size_t err_len) {
    set_rcs_print_destination(RCS_PRINT_TO_NULL);
    const char *file = nml_file && *nml_file ? nml_file : DEFAULT_EMC_NMLFILE;
    lcnc_t *c = new lcnc{};
    c->stat = new RCS_STAT_CHANNEL(emcFormat, "emcStatus", "xemc", file);
    c->cmd = new RCS_CMD_CHANNEL(emcFormat, "emcCommand", "xemc", file);
    c->err = new NML(nmlErrorFormat, "emcError", "xemc", file);
    if (!c->stat->valid() || !c->cmd->valid() || !c->err->valid()) {
        snprintf(err, err_len, "cannot connect to LinuxCNC NML channels (%s); is LinuxCNC running?", file);
        lcnc_close(c);
        return nullptr;
    }
    // Continue serial numbers after whatever another GUI last sent.
    if (c->stat->peek() == EMC_STAT_TYPE) {
        c->have_status = true;
        c->serial = ((EMC_STAT *)c->stat->get_address())->echo_serial_number;
    }
    return c;
}

extern "C" void lcnc_close(lcnc_t *c) {
    if (!c) return;
    delete c->stat;
    delete c->cmd;
    delete c->err;
    delete c;
}

extern "C" int lcnc_poll(lcnc_t *c, lcnc_status_t *o) {
    if (!c->stat->valid()) return -1;
    NMLTYPE type = c->stat->peek();
    if (type == EMC_STAT_TYPE) c->have_status = true;
    else if (type < 0 || (type == 0 && !c->have_status)) return -1;
    const EMC_STAT *s = (const EMC_STAT *)c->stat->get_address();
    memset(o, 0, sizeof(*o));

    const EMC_TASK_STAT &t = s->task;
    o->task_mode = t.mode;
    o->task_state = t.state;
    o->exec_state = t.execState;
    o->interp_state = t.interpState;
    o->call_level = t.callLevel;
    o->motion_line = t.motionLine;
    o->current_line = t.currentLine;
    o->read_line = t.readLine;
    o->optional_stop = t.optional_stop_state;
    o->block_delete = t.block_delete_state;
    o->task_paused = t.task_paused;
    o->queued_mdi = t.queuedMDIcommands;
    o->g5x_index = t.g5x_index;
    o->program_units = t.programUnits;
    o->interp_errcode = t.interpreter_errcode;
    o->delay_left = t.delayLeft;
    o->rotation_xy = t.rotation_xy;
    pose(o->g5x_offset, t.g5x_offset);
    pose(o->g92_offset, t.g92_offset);
    pose(o->tool_offset, t.toolOffset);
    for (int i = 0; i < LCNC_GCODES; i++) o->active_gcodes[i] = t.activeGCodes[i];
    for (int i = 0; i < LCNC_MCODES; i++) o->active_mcodes[i] = t.activeMCodes[i];
    for (int i = 0; i < LCNC_SETTINGS; i++) o->active_settings[i] = t.activeSettings[i];
    copy_str(o->file, LCNC_PATH, t.file);
    copy_str(o->command, LCNC_PATH, t.command);
    copy_str(o->ini_filename, LCNC_PATH, t.ini_filename);

    const EMC_TRAJ_STAT &j = s->motion.traj;
    o->joints = j.joints;
    o->axis_mask = j.axis_mask;
    o->traj_mode = j.mode;
    o->enabled = j.enabled;
    o->inpos = j.inpos;
    o->queue = j.queue;
    o->paused = j.paused;
    o->probe_tripped = j.probe_tripped;
    o->probing = j.probing;
    o->probeval = j.probeval;
    o->motion_type = j.motion_type;
    o->feed_hold_enabled = j.feed_hold_enabled;
    o->feed_override_enabled = j.feed_override_enabled;
    o->linear_units = j.linearUnits;
    o->angular_units = j.angularUnits;
    o->cycle_time = j.cycleTime;
    o->feed_scale = j.scale;
    o->rapid_scale = j.rapid_scale;
    o->current_vel = j.current_vel;
    o->distance_to_go = j.distance_to_go;
    o->max_velocity = j.maxVelocity;
    pose(o->position, j.position);
    pose(o->actual_position, j.actualPosition);
    pose(o->dtg, j.dtg);
    pose(o->probed_position, j.probedPosition);

    for (int i = 0; i < LCNC_JOINTS && i < EMCMOT_MAX_JOINTS; i++) {
        const EMC_JOINT_STAT &js = s->motion.joint[i];
        o->homed[i] = js.homed;
        o->homing[i] = js.homing;
        o->fault[i] = js.fault;
        o->min_soft_limit[i] = js.minSoftLimit;
        o->max_soft_limit[i] = js.maxSoftLimit;
        o->min_hard_limit[i] = js.minHardLimit;
        o->max_hard_limit[i] = js.maxHardLimit;
        o->ferror[i] = js.ferrorCurrent;
    }
    o->on_soft_limit = s->motion.on_soft_limit;

    const EMC_SPINDLE_STAT &sp = s->motion.spindle[0];
    o->spindle_speed = sp.speed;
    o->spindle_scale = sp.spindle_scale;
    o->spindle_direction = sp.direction;
    o->spindle_enabled = sp.enabled;
    o->spindle_brake = sp.brake;
    o->spindle_override_enabled = sp.spindle_override_enabled;

    o->estop = s->io.aux.estop;
    o->tool_in_spindle = s->io.tool.toolInSpindle;
    o->pocket_prepped = s->io.tool.pocketPrepped;
    o->mist = s->io.coolant.mist;
    o->flood = s->io.coolant.flood;

    o->echo_serial = s->echo_serial_number;
    o->command_status = s->status;
    return 0;
}

extern "C" int lcnc_next_error(lcnc_t *c, char *buf, size_t len) {
    if (!c->err->valid()) return -1;
    NMLTYPE type = c->err->read();
    if (type <= 0) return type == 0 ? 0 : -1;
    void *a = c->err->get_address();
    switch (type) {
    case EMC_OPERATOR_ERROR_TYPE: copy_str(buf, len, ((EMC_OPERATOR_ERROR *)a)->error); return 1;
    case EMC_OPERATOR_TEXT_TYPE: copy_str(buf, len, ((EMC_OPERATOR_TEXT *)a)->text); return 2;
    case EMC_OPERATOR_DISPLAY_TYPE: copy_str(buf, len, ((EMC_OPERATOR_DISPLAY *)a)->display); return 3;
    case NML_ERROR_TYPE: copy_str(buf, len, ((NML_ERROR *)a)->error); return 1;
    case NML_TEXT_TYPE: copy_str(buf, len, ((NML_TEXT *)a)->text); return 2;
    case NML_DISPLAY_TYPE: copy_str(buf, len, ((NML_DISPLAY *)a)->display); return 3;
    default: snprintf(buf, len, "unknown error message type %ld", (long)type); return 1;
    }
}

template <typename M> static int send(lcnc_t *c, M &m) {
    m.serial_number = ++c->serial;
    if (c->cmd->write(&m) != 0) return -1;
    return m.serial_number;
}

extern "C" int lcnc_set_state(lcnc_t *c, int state) {
    EMC_TASK_SET_STATE m; m.state = (EMC_TASK_STATE_ENUM)state; return send(c, m);
}
extern "C" int lcnc_set_mode(lcnc_t *c, int mode) {
    EMC_TASK_SET_MODE m; m.mode = (EMC_TASK_MODE_ENUM)mode; return send(c, m);
}
extern "C" int lcnc_abort(lcnc_t *c) { EMC_TASK_ABORT m; return send(c, m); }
extern "C" int lcnc_mdi(lcnc_t *c, const char *line) {
    EMC_TASK_PLAN_EXECUTE m; copy_str(m.command, sizeof(m.command), line); return send(c, m);
}
extern "C" int lcnc_program_open(lcnc_t *c, const char *path) {
    EMC_TASK_PLAN_OPEN m; copy_str(m.file, sizeof(m.file), path); return send(c, m);
}
extern "C" int lcnc_program_run(lcnc_t *c, int line) {
    EMC_TASK_PLAN_RUN m; m.line = line; return send(c, m);
}
extern "C" int lcnc_program_pause(lcnc_t *c) { EMC_TASK_PLAN_PAUSE m; return send(c, m); }
extern "C" int lcnc_program_resume(lcnc_t *c) { EMC_TASK_PLAN_RESUME m; return send(c, m); }
extern "C" int lcnc_program_step(lcnc_t *c) { EMC_TASK_PLAN_STEP m; return send(c, m); }
extern "C" int lcnc_set_optional_stop(lcnc_t *c, int on) {
    EMC_TASK_PLAN_SET_OPTIONAL_STOP m; m.state = on; return send(c, m);
}
extern "C" int lcnc_set_block_delete(lcnc_t *c, int on) {
    EMC_TASK_PLAN_SET_BLOCK_DELETE m; m.state = on; return send(c, m);
}
extern "C" int lcnc_home(lcnc_t *c, int joint) { EMC_JOINT_HOME m; m.joint = joint; return send(c, m); }
extern "C" int lcnc_unhome(lcnc_t *c, int joint) { EMC_JOINT_UNHOME m; m.joint = joint; return send(c, m); }
extern "C" int lcnc_teleop(lcnc_t *c, int on) {
    EMC_TRAJ_SET_TELEOP_ENABLE m; m.enable = on; return send(c, m);
}
extern "C" int lcnc_jog_cont(lcnc_t *c, int joint_mode, int index, double vel) {
    EMC_JOG_CONT m; m.jjogmode = joint_mode; m.joint_or_axis = index; m.vel = vel; return send(c, m);
}
extern "C" int lcnc_jog_incr(lcnc_t *c, int joint_mode, int index, double vel, double incr) {
    EMC_JOG_INCR m; m.jjogmode = joint_mode; m.joint_or_axis = index; m.vel = vel; m.incr = incr;
    return send(c, m);
}
extern "C" int lcnc_jog_stop(lcnc_t *c, int joint_mode, int index) {
    EMC_JOG_STOP m; m.jjogmode = joint_mode; m.joint_or_axis = index; return send(c, m);
}
extern "C" int lcnc_feed_scale(lcnc_t *c, double scale) {
    EMC_TRAJ_SET_SCALE m; m.scale = scale; return send(c, m);
}
extern "C" int lcnc_rapid_scale(lcnc_t *c, double scale) {
    EMC_TRAJ_SET_RAPID_SCALE m; m.scale = scale; return send(c, m);
}
extern "C" int lcnc_spindle_scale(lcnc_t *c, double scale) {
    EMC_TRAJ_SET_SPINDLE_SCALE m; m.spindle = 0; m.scale = scale; return send(c, m);
}
extern "C" int lcnc_spindle_on(lcnc_t *c, double rpm) {
    EMC_SPINDLE_ON m; m.spindle = 0; m.speed = rpm; m.factor = 0; m.xoffset = 0;
    m.wait_for_spindle_at_speed = 1;
    return send(c, m);
}
extern "C" int lcnc_spindle_off(lcnc_t *c) { EMC_SPINDLE_OFF m; m.spindle = 0; return send(c, m); }
extern "C" int lcnc_load_tool_table(lcnc_t *c) {
    EMC_TOOL_LOAD_TOOL_TABLE m; m.file[0] = 0; return send(c, m);
}
extern "C" int lcnc_task_plan_synch(lcnc_t *c) { EMC_TASK_PLAN_SYNCH m; return send(c, m); }
