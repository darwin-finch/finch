import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitGrantLicenseKeysProperties, BenefitGrantLicenseKeysProperties$Outbound } from "./benefitgrantlicensekeysproperties.js";
import { BenefitLicenseKeysSubscriber, BenefitLicenseKeysSubscriber$Outbound } from "./benefitlicensekeyssubscriber.js";
import { CustomerPortalCustomer, CustomerPortalCustomer$Outbound } from "./customerportalcustomer.js";
export type CustomerBenefitGrantLicenseKeys = {
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
    benefit: BenefitLicenseKeysSubscriber;
    properties: BenefitGrantLicenseKeysProperties;
};
/** @internal */
export declare const CustomerBenefitGrantLicenseKeys$inboundSchema: z.ZodType<CustomerBenefitGrantLicenseKeys, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerBenefitGrantLicenseKeys$Outbound = {
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
    benefit: BenefitLicenseKeysSubscriber$Outbound;
    properties: BenefitGrantLicenseKeysProperties$Outbound;
};
/** @internal */
export declare const CustomerBenefitGrantLicenseKeys$outboundSchema: z.ZodType<CustomerBenefitGrantLicenseKeys$Outbound, z.ZodTypeDef, CustomerBenefitGrantLicenseKeys>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerBenefitGrantLicenseKeys$ {
    /** @deprecated use `CustomerBenefitGrantLicenseKeys$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerBenefitGrantLicenseKeys, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerBenefitGrantLicenseKeys$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerBenefitGrantLicenseKeys$Outbound, z.ZodTypeDef, CustomerBenefitGrantLicenseKeys>;
    /** @deprecated use `CustomerBenefitGrantLicenseKeys$Outbound` instead. */
    type Outbound = CustomerBenefitGrantLicenseKeys$Outbound;
}
export declare function customerBenefitGrantLicenseKeysToJSON(customerBenefitGrantLicenseKeys: CustomerBenefitGrantLicenseKeys): string;
export declare function customerBenefitGrantLicenseKeysFromJSON(jsonString: string): SafeParseResult<CustomerBenefitGrantLicenseKeys, SDKValidationError>;
//# sourceMappingURL=customerbenefitgrantlicensekeys.d.ts.map