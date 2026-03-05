import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export declare const SwitchingFrom: {
    readonly Paddle: "paddle";
    readonly LemonSqueezy: "lemon_squeezy";
    readonly Gumroad: "gumroad";
    readonly Stripe: "stripe";
    readonly Other: "other";
};
export type SwitchingFrom = ClosedEnum<typeof SwitchingFrom>;
export type OrganizationDetails = {
    /**
     * Brief information about you and your business.
     */
    about: string;
    /**
     * Description of digital products being sold.
     */
    productDescription: string;
    /**
     * How the organization will integrate and use Polar.
     */
    intendedUse: string;
    /**
     * Main customer acquisition channels.
     */
    customerAcquisition: Array<string>;
    /**
     * Estimated revenue in the next 12 months
     */
    futureAnnualRevenue: number;
    /**
     * Switching from another platform?
     */
    switching?: boolean | undefined;
    /**
     * Which platform the organization is migrating from.
     */
    switchingFrom?: SwitchingFrom | null | undefined;
    /**
     * Revenue from last year if applicable.
     */
    previousAnnualRevenue?: number | undefined;
};
/** @internal */
export declare const SwitchingFrom$inboundSchema: z.ZodNativeEnum<typeof SwitchingFrom>;
/** @internal */
export declare const SwitchingFrom$outboundSchema: z.ZodNativeEnum<typeof SwitchingFrom>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace SwitchingFrom$ {
    /** @deprecated use `SwitchingFrom$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly Paddle: "paddle";
        readonly LemonSqueezy: "lemon_squeezy";
        readonly Gumroad: "gumroad";
        readonly Stripe: "stripe";
        readonly Other: "other";
    }>;
    /** @deprecated use `SwitchingFrom$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly Paddle: "paddle";
        readonly LemonSqueezy: "lemon_squeezy";
        readonly Gumroad: "gumroad";
        readonly Stripe: "stripe";
        readonly Other: "other";
    }>;
}
/** @internal */
export declare const OrganizationDetails$inboundSchema: z.ZodType<OrganizationDetails, z.ZodTypeDef, unknown>;
/** @internal */
export type OrganizationDetails$Outbound = {
    about: string;
    product_description: string;
    intended_use: string;
    customer_acquisition: Array<string>;
    future_annual_revenue: number;
    switching: boolean;
    switching_from?: string | null | undefined;
    previous_annual_revenue: number;
};
/** @internal */
export declare const OrganizationDetails$outboundSchema: z.ZodType<OrganizationDetails$Outbound, z.ZodTypeDef, OrganizationDetails>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace OrganizationDetails$ {
    /** @deprecated use `OrganizationDetails$inboundSchema` instead. */
    const inboundSchema: z.ZodType<OrganizationDetails, z.ZodTypeDef, unknown>;
    /** @deprecated use `OrganizationDetails$outboundSchema` instead. */
    const outboundSchema: z.ZodType<OrganizationDetails$Outbound, z.ZodTypeDef, OrganizationDetails>;
    /** @deprecated use `OrganizationDetails$Outbound` instead. */
    type Outbound = OrganizationDetails$Outbound;
}
export declare function organizationDetailsToJSON(organizationDetails: OrganizationDetails): string;
export declare function organizationDetailsFromJSON(jsonString: string): SafeParseResult<OrganizationDetails, SDKValidationError>;
//# sourceMappingURL=organizationdetails.d.ts.map