import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitGitHubRepositoryCreateProperties, BenefitGitHubRepositoryCreateProperties$Outbound } from "./benefitgithubrepositorycreateproperties.js";
export type BenefitGitHubRepositoryCreate = {
    type?: "github_repository" | undefined;
    /**
     * The description of the benefit. Will be displayed on products having this benefit.
     */
    description: string;
    /**
     * The ID of the organization owning the benefit. **Required unless you use an organization token.**
     */
    organizationId?: string | null | undefined;
    /**
     * Properties to create a benefit of type `github_repository`.
     */
    properties: BenefitGitHubRepositoryCreateProperties;
};
/** @internal */
export declare const BenefitGitHubRepositoryCreate$inboundSchema: z.ZodType<BenefitGitHubRepositoryCreate, z.ZodTypeDef, unknown>;
/** @internal */
export type BenefitGitHubRepositoryCreate$Outbound = {
    type: "github_repository";
    description: string;
    organization_id?: string | null | undefined;
    properties: BenefitGitHubRepositoryCreateProperties$Outbound;
};
/** @internal */
export declare const BenefitGitHubRepositoryCreate$outboundSchema: z.ZodType<BenefitGitHubRepositoryCreate$Outbound, z.ZodTypeDef, BenefitGitHubRepositoryCreate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace BenefitGitHubRepositoryCreate$ {
    /** @deprecated use `BenefitGitHubRepositoryCreate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<BenefitGitHubRepositoryCreate, z.ZodTypeDef, unknown>;
    /** @deprecated use `BenefitGitHubRepositoryCreate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<BenefitGitHubRepositoryCreate$Outbound, z.ZodTypeDef, BenefitGitHubRepositoryCreate>;
    /** @deprecated use `BenefitGitHubRepositoryCreate$Outbound` instead. */
    type Outbound = BenefitGitHubRepositoryCreate$Outbound;
}
export declare function benefitGitHubRepositoryCreateToJSON(benefitGitHubRepositoryCreate: BenefitGitHubRepositoryCreate): string;
export declare function benefitGitHubRepositoryCreateFromJSON(jsonString: string): SafeParseResult<BenefitGitHubRepositoryCreate, SDKValidationError>;
//# sourceMappingURL=benefitgithubrepositorycreate.d.ts.map