import { type ButtonHTMLAttributes, forwardRef } from "react";

type Variant = "primary" | "secondary" | "danger" | "success" | "ghost";
type Size = "sm" | "md";

interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  size?: Size;
  busy?: boolean;
}

// Solid fills use the stable, theme-independent accent tokens (defined in
// index.css) so the dark fill + white text stays readable on both light and
// dark surfaces. Outlined/ghost variants ride the zinc ramp, which the theme
// engine swaps automatically.
const variantCls: Record<Variant, string> = {
  primary:
    "bg-[var(--accent)] text-white hover:bg-[var(--accent-hover)]",
  secondary:
    "border border-zinc-700 bg-transparent text-zinc-300 hover:bg-zinc-800",
  danger:
    "bg-[var(--danger)] text-white hover:bg-[var(--danger-hover)]",
  success:
    "bg-[var(--success)] text-white hover:bg-[var(--success-hover)]",
  ghost:
    "bg-transparent text-zinc-400 hover:bg-zinc-900 hover:text-zinc-200",
};

const sizeCls: Record<Size, string> = {
  sm: "px-2.5 py-1 text-xs",
  md: "px-3 py-1.5 text-sm",
};

const Button = forwardRef<HTMLButtonElement, ButtonProps>(
  (
    {
      variant = "secondary",
      size = "md",
      busy = false,
      disabled,
      className = "",
      children,
      ...rest
    },
    ref,
  ) => {
    const isDisabled = disabled || busy;
    return (
      <button
        ref={ref}
        disabled={isDisabled}
        className={[
          "inline-flex items-center justify-center rounded-md font-medium transition",
          "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70",
          "disabled:cursor-not-allowed disabled:opacity-50",
          variantCls[variant],
          sizeCls[size],
          className,
        ]
          .filter(Boolean)
          .join(" ")}
        {...rest}
      >
        {children}
      </button>
    );
  },
);

Button.displayName = "Button";

export default Button;
