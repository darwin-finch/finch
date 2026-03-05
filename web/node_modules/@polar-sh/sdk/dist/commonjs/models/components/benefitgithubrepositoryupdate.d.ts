import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitGitHubRepositoryCreateProperties, BenefitGitHubRepositoryCreateProperties$Outbound } from "./benefitgithubrepositorycreateproperties.js";
export type BenefitGitHubRepositoryUpdate = {
    /**
     * The description of the benefit. Will be displayed on products having this benefit.
     */
    description?: string | null | undefined;
    type?: "github_repository" | undefined;
    properties?: BenefitGitHubRepositoryCreateProperties | null | undefined;
};
/** @internal */
export declare const BenefitGitHubRepositoryUpdate$inboundSchema: z.ZodType<BenefitGitHubRepositoryUpdate, z.ZodTypeDef, unknown>;
/** @internal */
export type BenefitGitHubRepositoryUpdate$Outbound = {
    description?: string | null | undefined;
    type: "github_repository";
    properties?: BenefitGitHubRepositoryCreateProperties$Outbound | null | undefined;
};
/** @internal */
export declare const BenefitGitHubRepositoryUpdate$outboundSchema: z.ZodType<BenefitGitHubRepositoryUpdate$Outbound, z.ZodTypeDef, BenefitGitHubRepositoryUpdate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace BenefitGitHubRepositoryUpdate$ {
    /** @deprecated use `BenefitGitHubRepositoryUpdate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<BenefitGitHubRepositoryUpdate, z.ZodTypeDef, unknown>;
    /** @deprecated use `BenefitGitHubRepositoryUpdate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<BenefitGitHubRepositoryUpdate$Outbound, z.ZodTypeDef, BenefitGitHubRepositoryUpdate>;
    /** @deprecated use `BenefitGitHubRepositoryUpdate$Outbound` instead. */
    type Outbound = BenefitGitHubRepositoryUpdate$Outbound;
}
export declare function benefitGitHubRepositoryUpdateToJSON(benefitGitHubRepositoryUpdate: BenefitGitHubRepositoryUpdate): string;
export declare function benefitGitHubRepositoryUpdateFromJSON(jsonString: string): SafeParseResult<BenefitGitHubRepositoryUpdate, SDKValidationError>;
//# sourceMappingURL=benefitgithubrepositoryupdate.d.ts.map