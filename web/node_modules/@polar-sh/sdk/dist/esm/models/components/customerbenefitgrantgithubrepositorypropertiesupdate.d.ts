import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerBenefitGrantGitHubRepositoryPropertiesUpdate = {
    accountId: string;
};
/** @internal */
export declare const CustomerBenefitGrantGitHubRepositoryPropertiesUpdate$inboundSchema: z.ZodType<CustomerBenefitGrantGitHubRepositoryPropertiesUpdate, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerBenefitGrantGitHubRepositoryPropertiesUpdate$Outbound = {
    account_id: string;
};
/** @internal */
export declare const CustomerBenefitGrantGitHubRepositoryPropertiesUpdate$outboundSchema: z.ZodType<CustomerBenefitGrantGitHubRepositoryPropertiesUpdate$Outbound, z.ZodTypeDef, CustomerBenefitGrantGitHubRepositoryPropertiesUpdate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerBenefitGrantGitHubRepositoryPropertiesUpdate$ {
    /** @deprecated use `CustomerBenefitGrantGitHubRepositoryPropertiesUpdate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerBenefitGrantGitHubRepositoryPropertiesUpdate, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerBenefitGrantGitHubRepositoryPropertiesUpdate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerBenefitGrantGitHubRepositoryPropertiesUpdate$Outbound, z.ZodTypeDef, CustomerBenefitGrantGitHubRepositoryPropertiesUpdate>;
    /** @deprecated use `CustomerBenefitGrantGitHubRepositoryPropertiesUpdate$Outbound` instead. */
    type Outbound = CustomerBenefitGrantGitHubRepositoryPropertiesUpdate$Outbound;
}
export declare function customerBenefitGrantGitHubRepositoryPropertiesUpdateToJSON(customerBenefitGrantGitHubRepositoryPropertiesUpdate: CustomerBenefitGrantGitHubRepositoryPropertiesUpdate): string;
export declare function customerBenefitGrantGitHubRepositoryPropertiesUpdateFromJSON(jsonString: string): SafeParseResult<CustomerBenefitGrantGitHubRepositoryPropertiesUpdate, SDKValidationError>;
//# sourceMappingURL=customerbenefitgrantgithubrepositorypropertiesupdate.d.ts.map