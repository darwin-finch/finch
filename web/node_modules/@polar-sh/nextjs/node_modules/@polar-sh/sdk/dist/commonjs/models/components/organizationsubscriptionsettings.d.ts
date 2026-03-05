import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { SubscriptionProrationBehavior } from "./subscriptionprorationbehavior.js";
export type OrganizationSubscriptionSettings = {
    allowMultipleSubscriptions: boolean;
    allowCustomerUpdates: boolean;
    prorationBehavior: SubscriptionProrationBehavior;
};
/** @internal */
export declare const OrganizationSubscriptionSettings$inboundSchema: z.ZodType<OrganizationSubscriptionSettings, z.ZodTypeDef, unknown>;
/** @internal */
export type OrganizationSubscriptionSettings$Outbound = {
    allow_multiple_subscriptions: boolean;
    allow_customer_updates: boolean;
    proration_behavior: string;
};
/** @internal */
export declare const OrganizationSubscriptionSettings$outboundSchema: z.ZodType<OrganizationSubscriptionSettings$Outbound, z.ZodTypeDef, OrganizationSubscriptionSettings>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace OrganizationSubscriptionSettings$ {
    /** @deprecated use `OrganizationSubscriptionSettings$inboundSchema` instead. */
    const inboundSchema: z.ZodType<OrganizationSubscriptionSettings, z.ZodTypeDef, unknown>;
    /** @deprecated use `OrganizationSubscriptionSettings$outboundSchema` instead. */
    const outboundSchema: z.ZodType<OrganizationSubscriptionSettings$Outbound, z.ZodTypeDef, OrganizationSubscriptionSettings>;
    /** @deprecated use `OrganizationSubscriptionSettings$Outbound` instead. */
    type Outbound = OrganizationSubscriptionSettings$Outbound;
}
export declare function organizationSubscriptionSettingsToJSON(organizationSubscriptionSettings: OrganizationSubscriptionSettings): string;
export declare function organizationSubscriptionSettingsFromJSON(jsonString: string): SafeParseResult<OrganizationSubscriptionSettings, SDKValidationError>;
//# sourceMappingURL=organizationsubscriptionsettings.d.ts.map