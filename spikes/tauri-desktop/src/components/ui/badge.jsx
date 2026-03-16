import { cn } from "../../lib/utils";

export function Badge({ className, ...props }) {
  return (
    <span
      className={cn(
        "inline-flex items-center rounded-full border border-border/70 bg-secondary/60 px-2 py-0.5 text-xs font-semibold",
        className
      )}
      {...props}
    />
  );
}
