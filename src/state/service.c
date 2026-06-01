/**
 * @file service.c
 * @brief Typio IPC Protocol v2 — transport-agnostic dispatch (ADR-0008).
 *
 * Resource-oriented JSON-RPC handlers:
 *   - hello
 *   - config.{get,set,unset,list,show,reload}
 *   - engine.{list,describe,use,next,invoke}
 *   - daemon.{status,stop,version}
 *   - events.subscribe (delegated to transport via callback)
 *
 * camelCase JSON throughout. No backwards-compat shims.
 */

#include "state/service.h"
#include "state/engine_placeholder.h"
#include "ipc/tip_json.h"
#include "ipc/tip_protocol.h"

#include "typio/abi/config.h"
#include "typio/runtime/instance.h"
#include "typio/runtime/registry.h"
#include "typio/schema/config_schema.h"
#include "typio/typio.h"
#include "typio_build_config.h"
#include "typio/abi/log.h"
#include "typio/abi/string.h"
#include "plugin_loader.h"

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

struct TypioStatusService {
    TypioInstance *instance;
    TypioStatusServiceRuntimeStateCallback runtime_state_callback;
    void *runtime_state_user_data;
    TypioStatusServiceStopCallback stop_callback;
    void *stop_user_data;
    TypioStatusServiceSubscribeCallback subscribe_callback;
    void *subscribe_user_data;
    struct TypioStateController *state_controller;
    time_t started_at;
};

/* ------------------------------------------------------------------ */
/*  Small helpers                                                     */
/* ------------------------------------------------------------------ */

static TypioRegistry *svc_registry(TypioStatusService *svc)
{
    return svc && svc->instance
        ? typio_instance_get_registry(svc->instance)
        : nullptr;
}

static TypioConfig *svc_config(TypioStatusService *svc)
{
    return svc && svc->instance ? typio_instance_get_config(svc->instance) : nullptr;
}

static const char *field_type_name(TypioFieldType t)
{
    switch (t) {
    case TYPIO_FIELD_STRING: return "string";
    case TYPIO_FIELD_INT:    return "int";
    case TYPIO_FIELD_BOOL:   return "bool";
    case TYPIO_FIELD_FLOAT:  return "float";
    }
    return "string";
}

static const char *engine_kind_name(TypioEngineType type)
{
    return type == TYPIO_ENGINE_TYPE_VOICE ? "voice" : "keyboard";
}

/* Errors */
enum {
    RPC_PARSE_ERROR      = -32700,
    RPC_INVALID_REQUEST  = -32600,
    RPC_METHOD_NOT_FOUND = -32601,
    RPC_INVALID_PARAMS   = -32602,
    RPC_INTERNAL_ERROR   = -32603,
};

/* ------------------------------------------------------------------ */
/*  hello                                                             */
/* ------------------------------------------------------------------ */

static char *handle_hello([[maybe_unused]] TypioStatusService *svc,
                          [[maybe_unused]] const char *params, int id)
{
    TipJsonBuilder *b = tip_json_builder_new();
    TIP_JSON_OBJ_START(b);
    TIP_JSON_KEY(b, "protocolVersion");
    tip_json_builder_append_int(b, TYPIO_IPC_PROTOCOL_VERSION);
    TIP_JSON_COMMA(b);
    TIP_JSON_KEY(b, "daemonVersion");
    tip_json_builder_append_string(b, TYPIO_VERSION);
    TIP_JSON_COMMA(b);
    TIP_JSON_KEY(b, "capabilities");
    TIP_JSON_ARR_START(b);
    tip_json_builder_append_string(b, "config");
    TIP_JSON_COMMA(b);
    tip_json_builder_append_string(b, "engine");
    TIP_JSON_COMMA(b);
    tip_json_builder_append_string(b, "daemon");
    TIP_JSON_COMMA(b);
    tip_json_builder_append_string(b, "events");
    TIP_JSON_ARR_END(b);
    TIP_JSON_OBJ_END(b);
    char *result = tip_json_builder_steal(b);
    char *wrapped = tip_json_build_response(id, result);
    free(result);
    return wrapped;
}

/* ------------------------------------------------------------------ */
/*  config.*                                                          */
/* ------------------------------------------------------------------ */

static void append_config_value(TipJsonBuilder *b, TypioConfig *cfg,
                                const char *key, TypioFieldType type)
{
    switch (type) {
    case TYPIO_FIELD_STRING: {
        const char *v = typio_config_get_string(cfg, key, "");
        tip_json_builder_append_string(b, v ? v : "");
        break;
    }
    case TYPIO_FIELD_INT:
        tip_json_builder_append_int(b, typio_config_get_int(cfg, key, 0));
        break;
    case TYPIO_FIELD_BOOL:
        tip_json_builder_append_bool(b, typio_config_get_bool(cfg, key, false));
        break;
    case TYPIO_FIELD_FLOAT:
        tip_json_builder_append_double(b, typio_config_get_float(cfg, key, 0.0));
        break;
    }
}

static char *handle_config_get(TypioStatusService *svc, const char *params, int id)
{
    char *key = tip_json_extract_string(params, "key");
    if (!key)
        return tip_json_build_error(id, RPC_INVALID_PARAMS, "Missing 'key' param");

    TypioConfig *cfg = svc_config(svc);
    if (!cfg) {
        free(key);
        return tip_json_build_error(id, RPC_INTERNAL_ERROR, "Config unavailable");
    }

    const TypioConfigField *field = typio_config_schema_find(key);
    bool has = typio_config_has_key(cfg, key);
    if (!field && !has) {
        free(key);
        return tip_json_build_error(id, RPC_INVALID_PARAMS, "Unknown key");
    }

    TypioFieldType type = field ? field->type : TYPIO_FIELD_STRING;
    TipJsonBuilder *b = tip_json_builder_new();
    TIP_JSON_OBJ_START(b);
    TIP_JSON_KEY(b, "value");
    append_config_value(b, cfg, key, type);
    TIP_JSON_COMMA(b);
    TIP_JSON_KEY(b, "type");
    tip_json_builder_append_string(b, field_type_name(type));
    TIP_JSON_COMMA(b);
    TIP_JSON_KEY(b, "source");
    tip_json_builder_append_string(b, has ? "user" : "default");
    TIP_JSON_OBJ_END(b);

    char *payload = tip_json_builder_steal(b);
    char *response = tip_json_build_response(id, payload);
    free(payload);
    free(key);
    return response;
}

/* engine namespace key, or NULL. Returned pointer is into `key`. */
static const char *engine_namespace(const char *key, char **out_engine_name)
{
    *out_engine_name = nullptr;
    const char *prefix = "engines.";
    size_t plen = strlen(prefix);
    if (strncmp(key, prefix, plen) != 0)
        return nullptr;
    const char *rest = key + plen;
    const char *dot = strchr(rest, '.');
    if (!dot || dot == rest)
        return nullptr;
    size_t name_len = (size_t)(dot - rest);
    char *name = malloc(name_len + 1);
    if (!name)
        return nullptr;
    memcpy(name, rest, name_len);
    name[name_len] = '\0';
    *out_engine_name = name;
    return dot + 1; /* sub-key after the engine name */
}

static char *handle_config_set(TypioStatusService *svc, const char *params, int id)
{
    char *key = tip_json_extract_string(params, "key");
    char *value_str = tip_json_extract_string(params, "value");
    if (!key || !value_str) {
        free(key); free(value_str);
        return tip_json_build_error(id, RPC_INVALID_PARAMS,
                                    "Missing 'key' or 'value' param");
    }

    TypioConfig *cfg = svc_config(svc);
    if (!cfg) {
        free(key); free(value_str);
        return tip_json_build_error(id, RPC_INTERNAL_ERROR, "Config unavailable");
    }

    const TypioConfigField *field = typio_config_schema_find(key);
    TypioFieldType type = field ? field->type : TYPIO_FIELD_STRING;
    TypioResult r = TYPIO_OK;
    switch (type) {
    case TYPIO_FIELD_STRING:
        r = typio_config_set_string(cfg, key, value_str);
        break;
    case TYPIO_FIELD_INT:
        r = typio_config_set_int(cfg, key, (int)strtol(value_str, nullptr, 10));
        break;
    case TYPIO_FIELD_BOOL: {
        bool b = (strcmp(value_str, "true") == 0 || strcmp(value_str, "1") == 0);
        r = typio_config_set_bool(cfg, key, b);
        break;
    }
    case TYPIO_FIELD_FLOAT:
        r = typio_config_set_float(cfg, key, strtod(value_str, nullptr));
        break;
    }
    if (r != TYPIO_OK) {
        free(key); free(value_str);
        return tip_json_build_error(id, RPC_INTERNAL_ERROR, "config.set failed");
    }

    typio_instance_save_config(svc->instance);

    /* If this key belongs to an engine's namespace, notify it. */
    char *engine_name = nullptr;
    if (engine_namespace(key, &engine_name)) {
        TypioRegistry *reg = svc_registry(svc);
        if (reg)
            typio_registry_notify_config_change(reg, engine_name, key, value_str);
    }
    free(engine_name);

    free(key); free(value_str);
    return tip_json_build_response(id, "{}");
}

static char *handle_config_unset(TypioStatusService *svc, const char *params, int id)
{
    char *key = tip_json_extract_string(params, "key");
    if (!key)
        return tip_json_build_error(id, RPC_INVALID_PARAMS, "Missing 'key' param");

    TypioConfig *cfg = svc_config(svc);
    if (!cfg) {
        free(key);
        return tip_json_build_error(id, RPC_INTERNAL_ERROR, "Config unavailable");
    }

    TypioResult r = typio_config_remove(cfg, key);
    if (r != TYPIO_OK) {
        free(key);
        return tip_json_build_error(id, RPC_INVALID_PARAMS, "Unknown key");
    }
    typio_instance_save_config(svc->instance);
    free(key);
    return tip_json_build_response(id, "{}");
}

static char *handle_config_list(TypioStatusService *svc, const char *params, int id)
{
    char *prefix = tip_json_extract_string(params, "prefix");
    if (!prefix) prefix = strdup("");

    TypioConfig *cfg = svc_config(svc);
    size_t count = 0;
    const TypioConfigField **fields =
        typio_config_schema_fields_with_prefix(prefix, &count);

    TipJsonBuilder *b = tip_json_builder_new();
    TIP_JSON_ARR_START(b);
    for (size_t i = 0; i < count; i++) {
        const TypioConfigField *f = fields[i];
        if (i > 0) TIP_JSON_COMMA(b);
        TIP_JSON_OBJ_START(b);
        TIP_JSON_KEY(b, "key");
        tip_json_builder_append_string(b, f->key);
        TIP_JSON_COMMA(b);
        TIP_JSON_KEY(b, "type");
        tip_json_builder_append_string(b, field_type_name(f->type));
        TIP_JSON_COMMA(b);
        TIP_JSON_KEY(b, "value");
        append_config_value(b, cfg, f->key, f->type);
        TIP_JSON_COMMA(b);
        TIP_JSON_KEY(b, "label");
        tip_json_builder_append_string(b, f->ui_label ? f->ui_label : "");
        TIP_JSON_COMMA(b);
        TIP_JSON_KEY(b, "section");
        tip_json_builder_append_string(b, f->ui_section ? f->ui_section : "");
        if (f->ui_options) {
            TIP_JSON_COMMA(b);
            TIP_JSON_KEY(b, "choices");
            TIP_JSON_ARR_START(b);
            for (size_t j = 0; f->ui_options[j]; j++) {
                if (j > 0) TIP_JSON_COMMA(b);
                tip_json_builder_append_string(b, f->ui_options[j]);
            }
            TIP_JSON_ARR_END(b);
        }
        TIP_JSON_OBJ_END(b);
    }
    TIP_JSON_ARR_END(b);
    if (fields)
        typio_config_schema_fields_with_prefix_free(fields, count);
    free(prefix);

    char *payload = tip_json_builder_steal(b);
    char *response = tip_json_build_response(id, payload);
    free(payload);
    return response;
}

static char *handle_config_show(TypioStatusService *svc,
                                [[maybe_unused]] const char *params, int id)
{
    char *text = svc && svc->instance
        ? typio_instance_get_config_text(svc->instance) : nullptr;
    TipJsonBuilder *b = tip_json_builder_new();
    TIP_JSON_OBJ_START(b);
    TIP_JSON_KEY(b, "text");
    tip_json_builder_append_string(b, text ? text : "");
    TIP_JSON_COMMA(b);
    TIP_JSON_KEY(b, "format");
    tip_json_builder_append_string(b, "toml");
    TIP_JSON_OBJ_END(b);
    typio_free_string(text);
    char *payload = tip_json_builder_steal(b);
    char *response = tip_json_build_response(id, payload);
    free(payload);
    return response;
}

static char *handle_config_reload(TypioStatusService *svc,
                                  [[maybe_unused]] const char *params, int id)
{
    if (!svc) return tip_json_build_error(id, RPC_INTERNAL_ERROR, "no service");
    TypioResult r = typio_instance_reload_config(svc->instance);
    if (r != TYPIO_OK)
        return tip_json_build_error(id, RPC_INTERNAL_ERROR, "config.reload failed");
    return tip_json_build_response(id, "{}");
}

/* ------------------------------------------------------------------ */
/*  engine.*                                                          */
/* ------------------------------------------------------------------ */

static void emit_engine_summary(TipJsonBuilder *b, TypioRegistry *reg,
                                const char *name, const char *active_keyboard,
                                const char *active_voice, TypioEngineType type)
{
    const TypioEngineInfo *info = typio_registry_get_engine_info(reg, name);
    const char *display = info && info->display_name && info->display_name[0]
                              ? info->display_name : name;
    bool is_active = false;
    if (type == TYPIO_ENGINE_TYPE_KEYBOARD)
        is_active = active_keyboard && strcmp(active_keyboard, name) == 0;
    else
        is_active = active_voice && strcmp(active_voice, name) == 0;

    TIP_JSON_OBJ_START(b);
    TIP_JSON_KEY(b, "name");
    tip_json_builder_append_string(b, name);
    TIP_JSON_COMMA(b);
    TIP_JSON_KEY(b, "kind");
    tip_json_builder_append_string(b, engine_kind_name(type));
    TIP_JSON_COMMA(b);
    TIP_JSON_KEY(b, "displayName");
    tip_json_builder_append_string(b, display);
    TIP_JSON_COMMA(b);
    TIP_JSON_KEY(b, "active");
    tip_json_builder_append_bool(b, is_active);
    TIP_JSON_OBJ_END(b);
    typio_engine_info_free((TypioEngineInfo *)info);
}

static char *handle_engine_list(TypioStatusService *svc,
                                [[maybe_unused]] const char *params, int id)
{
    TypioRegistry *reg = svc_registry(svc);
    if (!reg) return tip_json_build_error(id, RPC_INTERNAL_ERROR, "no registry");

    size_t kb_count = 0, voice_count = 0;
    char **kb = typio_registry_list_keyboards(reg, &kb_count);
    char **voice = typio_registry_list_voices(reg, &voice_count);
    char *active_kb = typio_registry_get_active_keyboard(reg);
    char *active_voice = typio_registry_get_active_voice(reg);

    TipJsonBuilder *b = tip_json_builder_new();
    TIP_JSON_ARR_START(b);
    bool first = true;
    for (size_t i = 0; i < kb_count; i++) {
        if (!first) TIP_JSON_COMMA(b);
        first = false;
        emit_engine_summary(b, reg, kb[i], active_kb, active_voice,
                            TYPIO_ENGINE_TYPE_KEYBOARD);
    }
    for (size_t i = 0; i < voice_count; i++) {
        if (!first) TIP_JSON_COMMA(b);
        first = false;
        emit_engine_summary(b, reg, voice[i], active_kb, active_voice,
                            TYPIO_ENGINE_TYPE_VOICE);
    }
    TIP_JSON_ARR_END(b);

    typio_free_string_array(kb, kb_count);
    typio_free_string_array(voice, voice_count);
    typio_free_string(active_kb);
    typio_free_string(active_voice);

    char *payload = tip_json_builder_steal(b);
    char *response = tip_json_build_response(id, payload);
    free(payload);
    return response;
}

static char *handle_engine_describe(TypioStatusService *svc, const char *params, int id)
{
    char *name = tip_json_extract_string(params, "name");
    if (!name)
        return tip_json_build_error(id, RPC_INVALID_PARAMS, "Missing 'name' param");

    TypioRegistry *reg = svc_registry(svc);
    if (!reg) {
        free(name);
        return tip_json_build_error(id, RPC_INTERNAL_ERROR, "no registry");
    }
    const TypioEngineInfo *info = typio_registry_get_engine_info(reg, name);
    if (!info) {
        free(name);
        return tip_json_build_error(id, RPC_INVALID_PARAMS, "Unknown engine");
    }

    TypioConfig *cfg = svc_config(svc);
    char prefix[256];
    snprintf(prefix, sizeof(prefix), "engines.%s.", name);

    size_t prop_count = 0;
    const TypioConfigField **props =
        typio_config_schema_fields_with_prefix(prefix, &prop_count);

    size_t cmd_count = 0;
    TypioEngineCommand *cmds = typio_registry_list_commands(reg, name, &cmd_count);

    TipJsonBuilder *b = tip_json_builder_new();
    TIP_JSON_OBJ_START(b);
    TIP_JSON_KEY(b, "name");
    tip_json_builder_append_string(b, name);
    TIP_JSON_COMMA(b);
    TIP_JSON_KEY(b, "kind");
    tip_json_builder_append_string(b, engine_kind_name(info->type));
    TIP_JSON_COMMA(b);
    TIP_JSON_KEY(b, "displayName");
    tip_json_builder_append_string(b,
        info->display_name && info->display_name[0] ? info->display_name : name);

    TIP_JSON_COMMA(b);
    TIP_JSON_KEY(b, "properties");
    TIP_JSON_ARR_START(b);
    for (size_t i = 0; i < prop_count; i++) {
        const TypioConfigField *f = props[i];
        if (i > 0) TIP_JSON_COMMA(b);
        TIP_JSON_OBJ_START(b);
        TIP_JSON_KEY(b, "key");
        tip_json_builder_append_string(b, f->key);
        TIP_JSON_COMMA(b);
        TIP_JSON_KEY(b, "type");
        tip_json_builder_append_string(b, field_type_name(f->type));
        TIP_JSON_COMMA(b);
        TIP_JSON_KEY(b, "value");
        append_config_value(b, cfg, f->key, f->type);
        TIP_JSON_COMMA(b);
        TIP_JSON_KEY(b, "label");
        tip_json_builder_append_string(b, f->ui_label ? f->ui_label : "");
        if (f->ui_options) {
            TIP_JSON_COMMA(b);
            TIP_JSON_KEY(b, "choices");
            TIP_JSON_ARR_START(b);
            for (size_t j = 0; f->ui_options[j]; j++) {
                if (j > 0) TIP_JSON_COMMA(b);
                tip_json_builder_append_string(b, f->ui_options[j]);
            }
            TIP_JSON_ARR_END(b);
        }
        TIP_JSON_OBJ_END(b);
    }
    TIP_JSON_ARR_END(b);

    TIP_JSON_COMMA(b);
    TIP_JSON_KEY(b, "commands");
    TIP_JSON_ARR_START(b);
    for (size_t i = 0; i < cmd_count; i++) {
        if (i > 0) TIP_JSON_COMMA(b);
        TIP_JSON_OBJ_START(b);
        TIP_JSON_KEY(b, "id");
        tip_json_builder_append_string(b, cmds[i].id ? cmds[i].id : "");
        TIP_JSON_COMMA(b);
        TIP_JSON_KEY(b, "label");
        tip_json_builder_append_string(b, cmds[i].label ? cmds[i].label : "");
        TIP_JSON_OBJ_END(b);
    }
    TIP_JSON_ARR_END(b);
    TIP_JSON_OBJ_END(b);

    if (props) typio_config_schema_fields_with_prefix_free(props, prop_count);
    if (cmds) typio_engine_command_list_free(cmds, cmd_count);
    typio_engine_info_free((TypioEngineInfo *)info);

    char *payload = tip_json_builder_steal(b);
    char *response = tip_json_build_response(id, payload);
    free(payload);
    free(name);
    return response;
}

static char *handle_engine_use(TypioStatusService *svc, const char *params, int id)
{
    char *name = tip_json_extract_string(params, "name");
    if (!name || !*name) {
        free(name);
        return tip_json_build_error(id, RPC_INVALID_PARAMS, "Missing 'name' param");
    }
    TypioRegistry *reg = svc_registry(svc);
    if (!reg) {
        free(name);
        return tip_json_build_error(id, RPC_INTERNAL_ERROR, "no registry");
    }
    const TypioEngineInfo *info = typio_registry_get_engine_info(reg, name);
    if (!info) {
        free(name);
        return tip_json_build_error(id, RPC_INVALID_PARAMS, "Unknown engine");
    }
    TypioResult r = info->type == TYPIO_ENGINE_TYPE_VOICE
                        ? typio_registry_set_active_voice(reg, name)
                        : typio_registry_set_active_keyboard(reg, name);
    typio_engine_info_free((TypioEngineInfo *)info);
    if (r != TYPIO_OK) {
        free(name);
        return tip_json_build_error(id, RPC_INTERNAL_ERROR, "engine.use failed");
    }
    if (svc && svc->instance)
        typio_instance_save_config(svc->instance);
    free(name);
    return tip_json_build_response(id, "{}");
}

static char *handle_engine_next(TypioStatusService *svc,
                                const char *params, int id)
{
    TypioRegistry *reg = svc_registry(svc);
    if (!reg) return tip_json_build_error(id, RPC_INTERNAL_ERROR, "no registry");

    char *kind = tip_json_extract_string(params, "kind");
    bool voice = kind && strcmp(kind, "voice") == 0;
    free(kind);

    TypioResult r = voice ? typio_registry_next_voice(reg)
                          : typio_registry_next_keyboard(reg);
    if (r != TYPIO_OK)
        return tip_json_build_error(id, RPC_INTERNAL_ERROR, "engine.next failed");

    char *active = voice ? typio_registry_get_active_voice(reg)
                         : typio_registry_get_active_keyboard(reg);
    TipJsonBuilder *b = tip_json_builder_new();
    TIP_JSON_OBJ_START(b);
    TIP_JSON_KEY(b, "active");
    tip_json_builder_append_string(b, active ? active : "");
    TIP_JSON_OBJ_END(b);
    typio_free_string(active);
    char *payload = tip_json_builder_steal(b);
    char *response = tip_json_build_response(id, payload);
    free(payload);
    return response;
}

static char *handle_engine_invoke(TypioStatusService *svc, const char *params, int id)
{
    char *name = tip_json_extract_string(params, "name");
    char *cmd = tip_json_extract_string(params, "command");
    if (!name || !cmd) {
        free(name); free(cmd);
        return tip_json_build_error(id, RPC_INVALID_PARAMS,
                                    "Missing 'name' or 'command' param");
    }
    TypioRegistry *reg = svc_registry(svc);
    if (!reg) {
        free(name); free(cmd);
        return tip_json_build_error(id, RPC_INTERNAL_ERROR, "no registry");
    }
    TypioResult r = typio_registry_invoke_command(reg, name, cmd);
    free(name); free(cmd);
    switch (r) {
    case TYPIO_OK:
        return tip_json_build_response(id, "{}");
    case TYPIO_ERROR_NOT_FOUND:
        return tip_json_build_error(id, RPC_INVALID_PARAMS, "Unknown engine or command");
    case TYPIO_ERROR_ENGINE_NOT_AVAILABLE:
        return tip_json_build_error(id, RPC_METHOD_NOT_FOUND,
                                    "Command not supported by engine");
    default:
        return tip_json_build_error(id, RPC_INTERNAL_ERROR, "engine.invoke failed");
    }
}

static char *handle_engine_load(TypioStatusService *svc, const char *params, int id)
{
    char *path = tip_json_extract_string(params, "path");
    if (!path || !*path) {
        free(path);
        return tip_json_build_error(id, RPC_INVALID_PARAMS, "Missing 'path' param");
    }
    TypioRegistry *reg = svc_registry(svc);
    if (!reg) {
        free(path);
        return tip_json_build_error(id, RPC_INTERNAL_ERROR, "no registry");
    }

    if (!typio_plugin_load_single(reg, path)) {
        free(path);
        return tip_json_build_error(id, RPC_INTERNAL_ERROR, "engine.load failed");
    }

    TipJsonBuilder *b = tip_json_builder_new();
    TIP_JSON_OBJ_START(b);
    TIP_JSON_KEY(b, "loaded");
    tip_json_builder_append_bool(b, true);
    TIP_JSON_COMMA(b);
    TIP_JSON_KEY(b, "path");
    tip_json_builder_append_string(b, path);
    TIP_JSON_OBJ_END(b);

    free(path);
    char *payload = tip_json_builder_steal(b);
    char *response = tip_json_build_response(id, payload);
    free(payload);
    return response;
}

static char *handle_engine_unload(TypioStatusService *svc, const char *params, int id)
{
    char *name = tip_json_extract_string(params, "name");
    if (!name || !*name) {
        free(name);
        return tip_json_build_error(id, RPC_INVALID_PARAMS, "Missing 'name' param");
    }
    TypioRegistry *reg = svc_registry(svc);
    if (!reg) {
        free(name);
        return tip_json_build_error(id, RPC_INTERNAL_ERROR, "no registry");
    }

    if (!typio_plugin_unload(reg, name)) {
        free(name);
        return tip_json_build_error(id, RPC_INVALID_PARAMS, "Unknown engine");
    }

    TipJsonBuilder *b = tip_json_builder_new();
    TIP_JSON_OBJ_START(b);
    TIP_JSON_KEY(b, "unloaded");
    tip_json_builder_append_bool(b, true);
    TIP_JSON_COMMA(b);
    TIP_JSON_KEY(b, "name");
    tip_json_builder_append_string(b, name);
    TIP_JSON_OBJ_END(b);

    free(name);
    char *payload = tip_json_builder_steal(b);
    char *response = tip_json_build_response(id, payload);
    free(payload);
    return response;
}

static char *handle_engine_reload(TypioStatusService *svc, const char *params, int id)
{
    char *name = tip_json_extract_string(params, "name");
    char *path = tip_json_extract_string(params, "path");
    if (!name || !*name) {
        free(name); free(path);
        return tip_json_build_error(id, RPC_INVALID_PARAMS, "Missing 'name' param");
    }
    TypioRegistry *reg = svc_registry(svc);
    if (!reg) {
        free(name); free(path);
        return tip_json_build_error(id, RPC_INTERNAL_ERROR, "no registry");
    }

    const char *const *engine_dirs = typio_engine_dirs_build(nullptr);
    if (!typio_plugin_reload(reg, name, path, engine_dirs)) {
        typio_engine_dirs_free(engine_dirs);
        free(name); free(path);
        return tip_json_build_error(id, RPC_INTERNAL_ERROR, "engine.reload failed");
    }
    typio_engine_dirs_free(engine_dirs);

    TipJsonBuilder *b = tip_json_builder_new();
    TIP_JSON_OBJ_START(b);
    TIP_JSON_KEY(b, "reloaded");
    tip_json_builder_append_bool(b, true);
    TIP_JSON_COMMA(b);
    TIP_JSON_KEY(b, "name");
    tip_json_builder_append_string(b, name);
    if (path) {
        TIP_JSON_COMMA(b);
        TIP_JSON_KEY(b, "path");
        tip_json_builder_append_string(b, path);
    }
    TIP_JSON_OBJ_END(b);

    free(name); free(path);
    char *payload = tip_json_builder_steal(b);
    char *response = tip_json_build_response(id, payload);
    free(payload);
    return response;
}

/* ------------------------------------------------------------------ */
/*  daemon.*                                                          */
/* ------------------------------------------------------------------ */

static char *handle_daemon_version([[maybe_unused]] TypioStatusService *svc,
                                   [[maybe_unused]] const char *params, int id)
{
    TipJsonBuilder *b = tip_json_builder_new();
    TIP_JSON_OBJ_START(b);
    TIP_JSON_KEY(b, "version");
    tip_json_builder_append_string(b, TYPIO_VERSION);
    TIP_JSON_OBJ_END(b);
    char *payload = tip_json_builder_steal(b);
    char *response = tip_json_build_response(id, payload);
    free(payload);
    return response;
}

static char *handle_daemon_stop(TypioStatusService *svc,
                                [[maybe_unused]] const char *params, int id)
{
    if (!svc || !svc->stop_callback)
        return tip_json_build_error(id, RPC_INTERNAL_ERROR, "stop unavailable");
    svc->stop_callback(svc->stop_user_data);
    return tip_json_build_response(id, "{}");
}

static char *handle_daemon_status(TypioStatusService *svc,
                                  [[maybe_unused]] const char *params, int id)
{
    TypioRegistry *reg = svc_registry(svc);
    char *active_kb = reg ? typio_registry_get_active_keyboard(reg) : nullptr;
    char *active_voice = reg ? typio_registry_get_active_voice(reg) : nullptr;
    time_t now = time(nullptr);

    TipJsonBuilder *b = tip_json_builder_new();
    TIP_JSON_OBJ_START(b);
    TIP_JSON_KEY(b, "version");
    tip_json_builder_append_string(b, TYPIO_VERSION);
    TIP_JSON_COMMA(b);
    TIP_JSON_KEY(b, "protocolVersion");
    tip_json_builder_append_int(b, TYPIO_IPC_PROTOCOL_VERSION);
    TIP_JSON_COMMA(b);
    TIP_JSON_KEY(b, "uptimeSeconds");
    tip_json_builder_append_int(b, (int)(now - (svc ? svc->started_at : now)));
    TIP_JSON_COMMA(b);
    TIP_JSON_KEY(b, "activeKeyboardEngine");
    tip_json_builder_append_string(b, active_kb ? active_kb : "");
    TIP_JSON_COMMA(b);
    TIP_JSON_KEY(b, "activeVoiceEngine");
    tip_json_builder_append_string(b, active_voice ? active_voice : "");

    if (svc && svc->runtime_state_callback) {
        TypioStatusRuntimeState state = {0};
        svc->runtime_state_callback(svc->runtime_state_user_data, &state);
        TIP_JSON_COMMA(b);
        TIP_JSON_KEY(b, "runtime");
        TIP_JSON_OBJ_START(b);
        TIP_JSON_KEY(b, "frontendBackend");
        tip_json_builder_append_string(b,
            state.frontend_backend ? state.frontend_backend : "");
        TIP_JSON_COMMA(b);
        TIP_JSON_KEY(b, "lifecyclePhase");
        tip_json_builder_append_string(b,
            state.lifecycle_phase ? state.lifecycle_phase : "");
        TIP_JSON_COMMA(b);
        TIP_JSON_KEY(b, "virtualKeyboardState");
        tip_json_builder_append_string(b,
            state.virtual_keyboard_state ? state.virtual_keyboard_state : "");
        TIP_JSON_COMMA(b);
        TIP_JSON_KEY(b, "keyboardGrabActive");
        tip_json_builder_append_bool(b, state.keyboard_grab_active);
        TIP_JSON_COMMA(b);
        TIP_JSON_KEY(b, "watchdogArmed");
        tip_json_builder_append_bool(b, state.watchdog_armed);
        TIP_JSON_OBJ_END(b);
    }
    TIP_JSON_OBJ_END(b);
    typio_free_string(active_kb);
    typio_free_string(active_voice);

    char *payload = tip_json_builder_steal(b);
    char *response = tip_json_build_response(id, payload);
    free(payload);
    return response;
}

/* ------------------------------------------------------------------ */
/*  events.subscribe                                                  */
/* ------------------------------------------------------------------ */

static char *handle_events_subscribe(TypioStatusService *svc,
                                     const char *params, int id,
                                     void *client_ctx)
{
    if (!svc || !svc->subscribe_callback)
        return tip_json_build_error(id, RPC_INTERNAL_ERROR,
                                    "subscriptions unavailable");

    /* Extract topics array; NULL = subscribe to all. */
    char *topics[16] = {0};
    int n = tip_json_extract_string_array(params, "topics", topics, 16);
    if (n < 0) n = 0;

    svc->subscribe_callback(svc->subscribe_user_data, client_ctx,
                            (const char *const *)topics, (size_t)n);

    for (int i = 0; i < n; i++) free(topics[i]);
    return tip_json_build_response(id, "{\"subscribed\":true}");
}

/* ------------------------------------------------------------------ */
/*  Top-level dispatch                                                */
/* ------------------------------------------------------------------ */

char *typio_status_service_handle(TypioStatusService *svc,
                                   const char *method,
                                   const char *params,
                                   int id,
                                   void *client_ctx)
{
    if (!svc || !method)
        return nullptr;

    if (strcmp(method, TYPIO_IPC_METHOD_HELLO) == 0)
        return handle_hello(svc, params, id);

    if (strcmp(method, TYPIO_IPC_METHOD_CONFIG_GET) == 0)
        return handle_config_get(svc, params, id);
    if (strcmp(method, TYPIO_IPC_METHOD_CONFIG_SET) == 0)
        return handle_config_set(svc, params, id);
    if (strcmp(method, TYPIO_IPC_METHOD_CONFIG_UNSET) == 0)
        return handle_config_unset(svc, params, id);
    if (strcmp(method, TYPIO_IPC_METHOD_CONFIG_LIST) == 0)
        return handle_config_list(svc, params, id);
    if (strcmp(method, TYPIO_IPC_METHOD_CONFIG_SHOW) == 0)
        return handle_config_show(svc, params, id);
    if (strcmp(method, TYPIO_IPC_METHOD_CONFIG_RELOAD) == 0)
        return handle_config_reload(svc, params, id);

    if (strcmp(method, TYPIO_IPC_METHOD_ENGINE_LIST) == 0)
        return handle_engine_list(svc, params, id);
    if (strcmp(method, TYPIO_IPC_METHOD_ENGINE_DESCRIBE) == 0)
        return handle_engine_describe(svc, params, id);
    if (strcmp(method, TYPIO_IPC_METHOD_ENGINE_USE) == 0)
        return handle_engine_use(svc, params, id);
    if (strcmp(method, TYPIO_IPC_METHOD_ENGINE_NEXT) == 0)
        return handle_engine_next(svc, params, id);
    if (strcmp(method, TYPIO_IPC_METHOD_ENGINE_INVOKE) == 0)
        return handle_engine_invoke(svc, params, id);
    if (strcmp(method, TYPIO_IPC_METHOD_ENGINE_LOAD) == 0)
        return handle_engine_load(svc, params, id);
    if (strcmp(method, TYPIO_IPC_METHOD_ENGINE_UNLOAD) == 0)
        return handle_engine_unload(svc, params, id);
    if (strcmp(method, TYPIO_IPC_METHOD_ENGINE_RELOAD) == 0)
        return handle_engine_reload(svc, params, id);

    if (strcmp(method, TYPIO_IPC_METHOD_DAEMON_STATUS) == 0)
        return handle_daemon_status(svc, params, id);
    if (strcmp(method, TYPIO_IPC_METHOD_DAEMON_STOP) == 0)
        return handle_daemon_stop(svc, params, id);
    if (strcmp(method, TYPIO_IPC_METHOD_DAEMON_VERSION) == 0)
        return handle_daemon_version(svc, params, id);

    if (strcmp(method, TYPIO_IPC_METHOD_EVENTS_SUBSCRIBE) == 0)
        return handle_events_subscribe(svc, params, id, client_ctx);

    return tip_json_build_error(id, RPC_METHOD_NOT_FOUND, "Method not found");
}

/* ------------------------------------------------------------------ */
/*  Public API                                                        */
/* ------------------------------------------------------------------ */

TypioStatusService *typio_status_service_new(TypioInstance *instance)
{
    if (!instance) return nullptr;
    TypioStatusService *svc = calloc(1, sizeof(*svc));
    if (!svc) return nullptr;
    svc->instance = instance;
    svc->started_at = time(nullptr);
    return svc;
}

void typio_status_service_destroy(TypioStatusService *svc)
{
    free(svc);
}

void typio_status_service_set_stop_callback(TypioStatusService *svc,
                                             TypioStatusServiceStopCallback cb,
                                             void *user_data)
{
    if (!svc) return;
    svc->stop_callback = cb;
    svc->stop_user_data = user_data;
}

void typio_status_service_set_runtime_state_callback(
    TypioStatusService *svc,
    TypioStatusServiceRuntimeStateCallback cb, void *user_data)
{
    if (!svc) return;
    svc->runtime_state_callback = cb;
    svc->runtime_state_user_data = user_data;
}

void typio_status_service_set_subscribe_callback(
    TypioStatusService *svc,
    TypioStatusServiceSubscribeCallback cb, void *user_data)
{
    if (!svc) return;
    svc->subscribe_callback = cb;
    svc->subscribe_user_data = user_data;
}

static void svc_state_change_callback([[maybe_unused]] void *user_data,
                                       [[maybe_unused]] TypioStateChangeType ct)
{
    /* The transport layer (IpcBus) listens to the state controller
     * directly and routes events.* notifications. The service has no
     * standing work to do here. */
}

void typio_status_service_bind_state_controller(TypioStatusService *svc,
                                                 TypioStateController *ctrl)
{
    if (!svc) return;
    if (svc->state_controller && svc->state_controller != ctrl)
        typio_state_controller_remove_listener(svc->state_controller, svc);
    svc->state_controller = ctrl;
    if (ctrl)
        typio_state_controller_add_listener(
            ctrl,
            (TypioStateListener){ .user_data = svc,
                                  .callback = svc_state_change_callback });
}
