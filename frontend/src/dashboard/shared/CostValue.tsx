import { CircleAlert } from "lucide-react";
import { NumberTicker } from "../../ui/beui/number-ticker";
import { Popover, PopoverContent, PopoverTrigger } from "../../ui/beui/popover";
import { formatCompactCost } from "../format";

export function CostValue({ value, unpriced, ticker = false, tickerBlur = false, className }: { value: number; unpriced: number; ticker?: boolean; tickerBlur?: boolean; className?: string }) {
  const text = value === null ? "—" : formatCompactCost(value);
  const valueNode = ticker
      ? <span title={`$${value.toFixed(2)}`}><NumberTicker value={Math.round(value * 100)} blur={tickerBlur} format={(next) => formatCompactCost(next / 100)} className={className} /></span>
      : <span className={className} title={`$${value.toFixed(2)}`}>{text}</span>;
  if (unpriced === 0) return valueNode;
  return <span className="inline-flex items-center justify-end gap-1.5">{valueNode}<Popover trigger="hover" side="bottom" align="end"><PopoverTrigger><button type="button" aria-label="未计价使用记录提示" className="inline-flex items-center justify-center text-warning outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2"><CircleAlert className="h-3.5 w-3.5" /></button></PopoverTrigger><PopoverContent inverseTheme className="w-max max-w-64 text-xs">{unpriced} 条使用记录未计价</PopoverContent></Popover></span>;
}
