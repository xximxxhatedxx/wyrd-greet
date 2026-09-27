-- Wyrd Greeter & Lock Screen Configuration (`~/.config/wyrd/greeter.lua` or `/etc/wyrd/greeter.lua`)
-- Uses the same declarative Lua DSL as `wyrd-shell` (`wyrd.theme`, `wyrd.config`, `wyrd.style`, `wyrd.surface`).

-- 1. Theme selection ("dynamic" automatically uses the live Material You palette from `wyrd-wallpaper`)
wyrd.theme("dynamic")

-- 2. High-level greeter layout & appearance options
wyrd.config({
    font = "MesloLGS Nerd Font, JetBrains Mono, sans-serif",
    clock_format = "%H:%M",
    date_format = "%A, %B %d",
    placeholder = "󰌋  Enter password...",
    show_top_bar = true,
    backdrop_dim = 0.56,
    card_width = 430.0,
    card_radius = 22.0,
})

-- 3. Optional style overrides (matches `wyrd-shell` style names:
--    "greet_top_bar", "bar_chip", "bar_chip_accent", "clock_time", "clock_date",
--    "greet_card", "greet_title", "greet_user", "greet_session", "greet_input")
-- Example:
-- wyrd.style("greet_card", {
--     radius = 24.0,
--     padding = { 28.0, 32.0, 28.0, 32.0 },
-- })

-- 4. Optional full custom widget tree builder via `wyrd.surface("greeter", { build = function(state) ... end })`
-- Uncomment below if you want to replace the entire layout with a custom Lua widget tree:
--[[
wyrd.surface("greeter", {
    build = function(state)
        return {
            type = "container",
            style = "greet_backdrop",
            layout = {
                mode = "flex_col",
                justify = "center",
                align = "center",
                gap = 24,
                width = state.width,
                height = state.height,
            },
            children = {
                {
                    type = "text",
                    style = "clock_time",
                    text = state.time,
                    layout = { width = 420, height = 68, justify = "center", align = "center" },
                },
                {
                    type = "container",
                    style = "greet_card",
                    layout = { mode = "flex_col", justify = "center", align = "center", gap = 14, width = 420, height = 260 },
                    children = {
                        { type = "button", style = "greet_user", text = "  " .. state.username, layout = { width = 260, height = 32 } },
                        { type = "button", style = "greet_session", text = "󰨡  " .. state.session_name, layout = { width = 300, height = 32 } },
                        { type = "button", style = "greet_input", text = state.password_len > 0 and state.password_masked or "󰌋  Enter password...", layout = { width = 340, height = 46 } },
                        { type = "text", style = state.auth_failed and "greet_status_err" or "greet_status_hint", text = state.status_text, layout = { width = 360, height = 22 } },
                    },
                },
            },
        }
    end,
})
]]
