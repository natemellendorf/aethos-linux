import { cn } from "../../lib/utils";

export function Input({ className, ...props }) {
  return (
    <input
      className={cn(
        "flex h-10 w-full rounded-md border border-input bg-background/80 px-3 py-2 text-sm placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/70",
        className
      )}
      {...props}
    />
  );
}
