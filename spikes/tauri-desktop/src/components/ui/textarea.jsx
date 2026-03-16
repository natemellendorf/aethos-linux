import { cn } from "../../lib/utils";

export function Textarea({ className, ...props }) {
  return (
    <textarea
      className={cn(
        "flex min-h-[80px] w-full rounded-md border border-input bg-background/80 px-3 py-2 text-sm placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/70",
        className
      )}
      {...props}
    />
  );
}
