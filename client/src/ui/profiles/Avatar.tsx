import type { ProfileStore } from "./profileStore";

export interface AvatarProps {
  profiles: ProfileStore;
  address: string;
  size?: "sm" | "md" | "lg";
  /** Default: the avatar is named by the participant's name and address. */
  decorative?: boolean;
  class?: string;
}

/** A participant's avatar image, with name and address as tooltip. */
export function Avatar({ profiles, address, size = "md", decorative = false, class: extra }: AvatarProps) {
  const profile = profiles.get(address);
  const classes = ["avatar", size === "md" ? "" : `avatar-${size}`, extra ?? ""].filter(Boolean).join(" ");
  const label = profile.name === address ? address : `${profile.name} (${address})`;
  return (
    <img
      class={classes}
      src={profile.imageUrl}
      alt={decorative ? "" : profile.name}
      title={label}
      loading="lazy"
      decoding="async"
      draggable={false}
    />
  );
}
