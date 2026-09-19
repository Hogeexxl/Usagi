import { usagiClient, type UsagiClient } from "../data/usagiClient";
import { StatefulButton } from "../ui/beui/button";
import { useUpdateController } from "./useUpdateController";

export function UpdateButton({ client = usagiClient }: { client?: UsagiClient }) {
  const view = useUpdateController({ client });
  if (!view.status?.update_available) return null;

  return (
    <StatefulButton
      state="idle"
      variant="primary"
      size="sm"
      ripple={false}
      onClick={view.open_release}
    >
      检测到新版本
    </StatefulButton>
  );
}
