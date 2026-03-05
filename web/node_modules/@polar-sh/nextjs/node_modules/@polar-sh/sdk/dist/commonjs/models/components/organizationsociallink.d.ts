import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { OrganizationSocialPlatforms } from "./organizationsocialplatforms.js";
export type OrganizationSocialLink = {
    platform: OrganizationSocialPlatforms;
    /**
     * The URL to the organization profile
     */
    url: string;
};
/** @internal */
export declare const OrganizationSocialLink$inboundSchema: z.ZodType<OrganizationSocialLink, z.ZodTypeDef, unknown>;
/** @internal */
export type OrganizationSocialLink$Outbound = {
    platform: string;
    url: string;
};
/** @internal */
export declare const OrganizationSocialLink$outboundSchema: z.ZodType<OrganizationSocialLink$Outbound, z.ZodTypeDef, OrganizationSocialLink>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace OrganizationSocialLink$ {
    /** @deprecated use `OrganizationSocialLink$inboundSchema` instead. */
    const inboundSchema: z.ZodType<OrganizationSocialLink, z.ZodTypeDef, unknown>;
    /** @deprecated use `OrganizationSocialLink$outboundSchema` instead. */
    const outboundSchema: z.ZodType<OrganizationSocialLink$Outbound, z.ZodTypeDef, OrganizationSocialLink>;
    /** @deprecated use `OrganizationSocialLink$Outbound` instead. */
    type Outbound = OrganizationSocialLink$Outbound;
}
export declare function organizationSocialLinkToJSON(organizationSocialLink: OrganizationSocialLink): string;
export declare function organizationSocialLinkFromJSON(jsonString: string): SafeParseResult<OrganizationSocialLink, SDKValidationError>;
//# sourceMappingURL=organizationsociallink.d.ts.map