import { Moon, Sun } from "lucide-react";

import { ActionSwapIcon } from "../ui/beui/action-swap";
import { Button } from "../ui/beui/button";
import { useThemeToggle } from "../ui/beui/theme-toggle";

export function DashboardThemeToggle() {
  const themeToggle = useThemeToggle({ variant: "circle-blur", start: "bottom-up" });

  return (
    <Button
      variant="ghost"
      size="icon"
      aria-label={themeToggle.mounted && themeToggle.isDark ? "Switch to light mode" : "Switch to dark mode"}
      onClick={themeToggle.toggle}
    >
      {themeToggle.mounted ? (
        <ActionSwapIcon value={themeToggle.isDark ? "dark" : "light"} animation="blur" className="h-4 w-4">
          {themeToggle.isDark ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />}
        </ActionSwapIcon>
      ) : (
        <span className="h-4 w-4" aria-hidden="true" />
      )}
    </Button>
  );
}
