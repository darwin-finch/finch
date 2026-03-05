import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitCustomSubscriber, BenefitCustomSubscriber$Outbound } from "./benefitcustomsubscriber.js";
import { BenefitGrantCustomProperties, BenefitGrantCustomProperties$Outbound } from "./benefitgrantcustomproperties.js";
import { CustomerPortalCustomer, CustomerPortalCustomer$Outbound } from "./customerportalcustomer.js";
export type CustomerBenefitGrantCustom = {
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
    benefit: BenefitCustomSubscriber;
    properties: BenefitGrantCustomProperties;
};
/** @internal */
export declare const CustomerBenefitGrantCustom$inboundSchema: z.ZodType<CustomerBenefitGrantCustom, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerBenefitGrantCustom$Outbound = {
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
    benefit: BenefitCustomSubscriber$Outbound;
    properties: BenefitGrantCustomProperties$Outbound;
};
/** @internal */
export declare const CustomerBenefitGrantCustom$outboundSchema: z.ZodType<CustomerBenefitGrantCustom$Outbound, z.ZodTypeDef, CustomerBenefitGrantCustom>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerBenefitGrantCustom$ {
    /** @deprecated use `CustomerBenefitGrantCustom$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerBenefitGrantCustom, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerBenefitGrantCustom$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerBenefitGrantCustom$Outbound, z.ZodTypeDef, CustomerBenefitGrantCustom>;
    /** @deprecated use `CustomerBenefitGrantCustom$Outbound` instead. */
    type Outbound = CustomerBenefitGrantCustom$Outbound;
}
export declare function customerBenefitGrantCustomToJSON(customerBenefitGrantCustom: CustomerBenefitGrantCustom): string;
export declare function customerBenefitGrantCustomFromJSON(jsonString: string): SafeParseResult<CustomerBenefitGrantCustom, SDKValidationError>;
//# sourceMappingURL=customerbenefitgrantcustom.d.ts.map