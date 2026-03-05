import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitDownloadablesSubscriber, BenefitDownloadablesSubscriber$Outbound } from "./benefitdownloadablessubscriber.js";
import { BenefitGrantDownloadablesProperties, BenefitGrantDownloadablesProperties$Outbound } from "./benefitgrantdownloadablesproperties.js";
import { CustomerPortalCustomer, CustomerPortalCustomer$Outbound } from "./customerportalcustomer.js";
export type CustomerBenefitGrantDownloadables = {
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
    benefit: BenefitDownloadablesSubscriber;
    properties: BenefitGrantDownloadablesProperties;
};
/** @internal */
export declare const CustomerBenefitGrantDownloadables$inboundSchema: z.ZodType<CustomerBenefitGrantDownloadables, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerBenefitGrantDownloadables$Outbound = {
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
    benefit: BenefitDownloadablesSubscriber$Outbound;
    properties: BenefitGrantDownloadablesProperties$Outbound;
};
/** @internal */
export declare const CustomerBenefitGrantDownloadables$outboundSchema: z.ZodType<CustomerBenefitGrantDownloadables$Outbound, z.ZodTypeDef, CustomerBenefitGrantDownloadables>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerBenefitGrantDownloadables$ {
    /** @deprecated use `CustomerBenefitGrantDownloadables$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerBenefitGrantDownloadables, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerBenefitGrantDownloadables$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerBenefitGrantDownloadables$Outbound, z.ZodTypeDef, CustomerBenefitGrantDownloadables>;
    /** @deprecated use `CustomerBenefitGrantDownloadables$Outbound` instead. */
    type Outbound = CustomerBenefitGrantDownloadables$Outbound;
}
export declare function customerBenefitGrantDownloadablesToJSON(customerBenefitGrantDownloadables: CustomerBenefitGrantDownloadables): string;
export declare function customerBenefitGrantDownloadablesFromJSON(jsonString: string): SafeParseResult<CustomerBenefitGrantDownloadables, SDKValidationError>;
//# sourceMappingURL=customerbenefitgrantdownloadables.d.ts.map