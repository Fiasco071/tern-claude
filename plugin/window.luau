--!strict
-- claude, window half: palette commands that open the tern-claude binary in a
-- new tab or beside the focused pane. Same shape as tern-azdo's window.luau.

local GROUP = "Claude"

local function launch(cx: WindowCx, how: "tab" | "split")
	if how == "split" then
		local pane = cx.session:focused()
		if pane then
			cx.layout:split(pane, "right", { command = "tern-claude" })
			return
		end
	end
	cx.layout:new_tab({ command = "tern-claude" })
end

tern.command({
	id = "open",
	title = "Claude: Sessions",
	icon = "sparkle",
	group = GROUP,
	run = function(cx)
		launch(cx, "tab")
	end,
})

tern.command({
	id = "open_split",
	title = "Claude: Sessions Beside Current Pane",
	icon = "sparkle",
	group = GROUP,
	run = function(cx)
		launch(cx, "split")
	end,
})
