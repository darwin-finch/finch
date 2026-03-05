import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitGitHubRepositorySubscriber, BenefitGitHubRepositorySubscriber$Outbound } from "./benefitgithubrepositorysubscriber.js";
import { BenefitGrantGitHubRepositoryProperties, BenefitGrantGitHubRepositoryProperties$Outbound } from "./benefitgrantgithubrepositoryproperties.js";
import { CustomerPortalCustomer, CustomerPortalCustomer$Outbound } from "./customerportalcustomer.js";
export type CustomerBenefitGrantGitHubRepository = {
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
    benefit: BenefitGitHubRepositorySubscriber;
    properties: BenefitGrantGitHubRepositoryProperties;
};
/** @internal */
export declare const CustomerBenefitGrantGitHubRepository$inboundSchema: z.ZodType<CustomerBenefitGrantGitHubRepository, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerBenefitGrantGitHubRepository$Outbound = {
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
    benefit: BenefitGitHubRepositorySubscriber$Outbound;
    properties: BenefitGrantGitHubRepositoryProperties$Outbound;
};
/** @internal */
export declare const CustomerBenefitGrantGitHubRepository$outboundSchema: z.ZodType<CustomerBenefitGrantGitHubRepository$Outbound, z.ZodTypeDef, CustomerBenefitGrantGitHubRepository>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerBenefitGrantGitHubRepository$ {
    /** @deprecated use `CustomerBenefitGrantGitHubRepository$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerBenefitGrantGitHubRepository, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerBenefitGrantGitHubRepository$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerBenefitGrantGitHubRepository$Outbound, z.ZodTypeDef, CustomerBenefitGrantGitHubRepository>;
    /** @deprecated use `CustomerBenefitGrantGitHubRepository$Outbound` instead. */
    type Outbound = CustomerBenefitGrantGitHubRepository$Outbound;
}
export declare function customerBenefitGrantGitHubRepositoryToJSON(customerBenefitGrantGitHubRepository: CustomerBenefitGrantGitHubRepository): string;
export declare function customerBenefitGrantGitHubRepositoryFromJSON(jsonString: string): SafeParseResult<CustomerBenefitGrantGitHubRepository, SDKValidationError>;
//# sourceMappingURL=customerbenefitgrantgithubrepository.d.ts.map