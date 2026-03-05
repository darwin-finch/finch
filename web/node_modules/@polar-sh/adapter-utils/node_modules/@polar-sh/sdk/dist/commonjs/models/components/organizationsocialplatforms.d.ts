import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
export declare const OrganizationSocialPlatforms: {
    readonly X: "x";
    readonly Github: "github";
    readonly Facebook: "facebook";
    readonly Instagram: "instagram";
    readonly Youtube: "youtube";
    readonly Tiktok: "tiktok";
    readonly Linkedin: "linkedin";
    readonly Other: "other";
};
export type OrganizationSocialPlatforms = ClosedEnum<typeof OrganizationSocialPlatforms>;
/** @internal */
export declare const OrganizationSocialPlatforms$inboundSchema: z.ZodNativeEnum<typeof OrganizationSocialPlatforms>;
/** @internal */
export declare const OrganizationSocialPlatforms$outboundSchema: z.ZodNativeEnum<typeof OrganizationSocialPlatforms>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace OrganizationSocialPlatforms$ {
    /** @deprecated use `OrganizationSocialPlatforms$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly X: "x";
        readonly Github: "github";
        readonly Facebook: "facebook";
        readonly Instagram: "instagram";
        readonly Youtube: "youtube";
        readonly Tiktok: "tiktok";
        readonly Linkedin: "linkedin";
        readonly Other: "other";
    }>;
    /** @deprecated use `OrganizationSocialPlatforms$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly X: "x";
        readonly Github: "github";
        readonly Facebook: "facebook";
        readonly Instagram: "instagram";
        readonly Youtube: "youtube";
        readonly Tiktok: "tiktok";
        readonly Linkedin: "linkedin";
        readonly Other: "other";
    }>;
}
//# sourceMappingURL=organizationsocialplatforms.d.ts.map