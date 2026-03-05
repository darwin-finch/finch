import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitDiscordSubscriber, BenefitDiscordSubscriber$Outbound } from "./benefitdiscordsubscriber.js";
import { BenefitGrantDiscordProperties, BenefitGrantDiscordProperties$Outbound } from "./benefitgrantdiscordproperties.js";
import { CustomerPortalCustomer, CustomerPortalCustomer$Outbound } from "./customerportalcustomer.js";
export type CustomerBenefitGrantDiscord = {
    /**
     * Creation timestamp of the object.
     */
    createdAt: Date;
    /**
     * Last modification timestamp of the object.
     */
    modifiedAt: Date | null;
    /**
     * The ID of the object.
     */
    id: string;
    grantedAt: Date | null;
    revokedAt: Date | null;
    customerId: string;
    benefitId: string;
    subscriptionId: string | null;
    orderId: string | null;
    isGranted: boolean;
    isRevoked: boolean;
    customer: CustomerPortalCustomer;
    benefit: BenefitDiscordSubscriber;
    properties: BenefitGrantDiscordProperties;
};
/** @internal */
export declare const CustomerBenefitGrantDiscord$inboundSchema: z.ZodType<CustomerBenefitGrantDiscord, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerBenefitGrantDiscord$Outbound = {
    created_at: string;
    modified_at: string | null;
    id: string;
    granted_at: string | null;
    revoked_at: string | null;
    customer_id: string;
    benefit_id: string;
    subscription_id: string | null;
    order_id: string | null;
    is_granted: boolean;
    is_revoked: boolean;
    customer: CustomerPortalCustomer$Outbound;
    benefit: BenefitDiscordSubscriber$Outbound;
    properties: BenefitGrantDiscordProperties$Outbound;
};
/** @internal */
export declare const CustomerBenefitGrantDiscord$outboundSchema: z.ZodType<CustomerBenefitGrantDiscord$Outbound, z.ZodTypeDef, CustomerBenefitGrantDiscord>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerBenefitGrantDiscord$ {
    /** @deprecated use `CustomerBenefitGrantDiscord$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerBenefitGrantDiscord, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerBenefitGrantDiscord$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerBenefitGrantDiscord$Outbound, z.ZodTypeDef, CustomerBenefitGrantDiscord>;
    /** @deprecated use `CustomerBenefitGrantDiscord$Outbound` instead. */
    type Outbound = CustomerBenefitGrantDiscord$Outbound;
}
export declare function customerBenefitGrantDiscordToJSON(customerBenefitGrantDiscord: CustomerBenefitGrantDiscord): string;
export declare function customerBenefitGrantDiscordFromJSON(jsonString: string): SafeParseResult<CustomerBenefitGrantDiscord, SDKValidationError>;
//# sourceMappingURL=customerbenefitgrantdiscord.d.ts.map